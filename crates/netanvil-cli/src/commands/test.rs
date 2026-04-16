use std::time::Duration;

use anyhow::{Context, Result};

use netanvil_api::{ControlServer, SharedState};
use netanvil_core::{report::ProgressLine, GenericTestBuilder, TestBuilder};
use netanvil_http::{HttpExecutor, Observe, ObserveConfig, SendObserveConfig};
use netanvil_types::{ConnectionConfig, RateConfig, TestConfig};

use crate::output;
use crate::parsing::*;

#[allow(clippy::too_many_arguments)]
pub fn run(
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
    scheduler: String,
    cores: usize,
    timeout: String,
    headers: Vec<String>,
    error_threshold: u16,
    pid_constraints: Vec<String>,
    pid_autotune_duration: String,
    connection_policy: String,
    persistent_ratio: f64,
    conn_lifetime: Option<String>,
    output: String,
    api_port: Option<u16>,
    http_version: String,
    bandwidth: Option<String>,
    response_signals: Vec<String>,
    payload: Option<String>,
    payload_hex: Option<String>,
    payload_file: Option<String>,
    framing: String,
    delimiter: String,
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
    event_log_dir: Option<String>,
    event_sample_rate: f64,
    health_sample_rate: f64,
    control_trace: Option<String>,
    latency_us: Option<u32>,
    error_rate: Option<u32>,
) -> Result<()> {
    let duration = parse_duration(&duration).context("invalid --duration")?;
    let timeout = parse_duration(&timeout).context("invalid --timeout")?;
    let bandwidth_bps = bandwidth
        .as_deref()
        .map(parse_bandwidth)
        .transpose()
        .context("invalid --bandwidth")?;
    let http_version = parse_http_version(&http_version).context("invalid --http-version")?;
    let autotune_dur =
        parse_duration(&pid_autotune_duration).context("invalid --pid-autotune-duration")?;
    let scheduler = parse_scheduler(&scheduler).context("invalid --scheduler")?;
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
            baseline_floor_ms: baseline_floor,
        },
        &AdaptiveShortcutArgs {
            latency_limit_ms: latency_limit,
            error_rate_limit_pct: error_rate_limit,
            latency_setpoint_ms: latency_setpoint,
            config_file: rate_config.clone(),
        },
    )?;

    let conn_policy = parse_connection_policy(
        &connection_policy,
        persistent_ratio,
        conn_lifetime.as_deref(),
    )?;

    let mut config = TestConfig {
        targets: url,
        method,
        duration,
        rate,
        scheduler,
        headers,
        num_cores: cores,
        connections: ConnectionConfig {
            request_timeout: timeout,
            connection_policy: conn_policy,
            ..Default::default()
        },
        control_interval: Duration::from_secs(3),
        error_status_threshold: error_threshold,
        bandwidth_limit_bps: bandwidth_bps,
        http_version,
        response_signal_headers: parse_response_signals(&response_signals)?,
        ..Default::default()
    };

    let rate_desc = match &config.rate {
        RateConfig::Static { rps } => format!("{rps} RPS (static)"),
        RateConfig::Step { steps } => format!("{} steps", steps.len()),
        RateConfig::Adaptive {
            bounds,
            constraints,
            warmup,
            initial_rps,
            ..
        } => {
            if let Some(w) = warmup {
                format!(
                    "{:.0} RPS warmup (adaptive, {:.0}-{:.0} RPS, {} constraints)",
                    w.rps,
                    bounds.min_rps,
                    bounds.max_rps,
                    constraints.len()
                )
            } else {
                format!(
                    "{:.0} RPS initial (adaptive, {:.0}-{:.0} RPS, {} constraints)",
                    initial_rps.unwrap_or(bounds.min_rps),
                    bounds.min_rps,
                    bounds.max_rps,
                    constraints.len()
                )
            }
        }
    };

    let bw_desc = match bandwidth_bps {
        Some(bps) => format!(", bandwidth: {}", format_bandwidth(bps)),
        None => String::new(),
    };
    eprintln!(
        "Running load test: {} target(s), {rate_desc}, {:?} duration, {} cores{bw_desc}",
        config.targets.len(),
        duration,
        if cores == 0 {
            "auto".to_string()
        } else {
            cores.to_string()
        },
    );

    let request_timeout = timeout;

    // Build event recorder factory if --event-log-dir is set
    let event_recorder_factory: Option<netanvil_core::EventRecorderFactory> =
        if let Some(ref dir) = event_log_dir {
            let protocol_name = detect_protocol_name(&config);
            let schema = netanvil_events::EventSchemaBuilder::new(protocol_name)
                .with_all()
                .sample_rate(event_sample_rate)
                .build();
            let anchor = netanvil_events::TimeAnchor::now();
            eprintln!(
                "  event log: {dir} (sample rate: {:.0}%)",
                event_sample_rate * 100.0
            );
            Some(schema.into_factory(dir.clone(), anchor))
        } else {
            None
        };

    if let Some(ref path) = control_trace {
        eprintln!("  control trace: {path}");
    }
    config.control_trace = control_trace;

    let protocol = detect_protocol(&config.targets);

    let result = match protocol {
        DetectedProtocol::Tcp => {
            let tcp_payload = resolve_payload(&payload, &payload_hex, &payload_file)
                .context("invalid TCP payload")?;
            let tcp_framing =
                parse_tcp_framing(&framing, &delimiter).context("invalid --framing")?;
            let targets = parse_socket_targets(&config.targets, "tcp://")
                .context("invalid TCP target address")?;
            let tcp_mode = parse_tcp_mode(&mode).context("invalid --mode")?;
            let expect_response = !no_response;

            // Parse size distributions (bare integer or distribution syntax)
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

            eprintln!(
                "  protocol: TCP, mode: {mode}, payload: {} bytes, request_size: {:?}, response_size: {:?}",
                tcp_payload.len(), req_size_dist, resp_size_dist,
            );

            config.error_status_threshold = 0; // no HTTP status for TCP
            config.protocol = Some(netanvil_types::ProtocolConfig::Tcp {
                mode: mode.clone(),
                payload_hex: encode_hex(&tcp_payload),
                framing: framing.clone(),
                request_size: req_size_dist.clone(),
                response_size: resp_size_dist.clone(),
            });

            let conn_policy = config.connections.connection_policy.clone();
            let max_conns = config.connections.max_connections_per_core;

            let gen_factory: netanvil_core::GenericGeneratorFactory<netanvil_tcp::TcpRequestSpec> =
                if let Some(ref plugin_path) = plugin {
                    let ptype = detect_plugin_type(plugin_path, &plugin_type)?;
                    tracing::info!(
                        plugin = %plugin_path,
                        plugin_type = ?ptype,
                        "loaded TCP plugin generator"
                    );
                    build_tcp_plugin_factory(plugin_path, ptype, &config.targets)?
                } else {
                    Box::new(move |_core_id| {
                        let mut gen = netanvil_tcp::SimpleTcpGenerator::new(
                            targets.clone(),
                            tcp_payload.clone(),
                            tcp_framing.clone(),
                            expect_response,
                        )
                        .with_mode(tcp_mode)
                        .with_request_size_dist(req_size_dist.clone())
                        .with_response_size_dist(resp_size_dist.clone());
                        if let Some(us) = latency_us {
                            gen = gen.with_latency_us(us);
                        }
                        if let Some(rate) = error_rate {
                            gen = gen.with_error_rate(rate);
                        }
                        Box::new(gen)
                    })
                };
            let trans_factory: netanvil_core::GenericTransformerFactory<
                netanvil_tcp::TcpRequestSpec,
            > = Box::new(|_| Box::new(netanvil_tcp::TcpNoopTransformer));

            let mut builder = GenericTestBuilder::new(
                config,
                move |_| {
                    netanvil_tcp::TcpExecutor::with_pool(
                        request_timeout,
                        max_conns,
                        conn_policy.clone(),
                    )
                },
                gen_factory,
                trans_factory,
            )
            .on_progress(|update| {
                eprint!("\r{}", ProgressLine::new(update));
            });
            if let Some(factory) = event_recorder_factory {
                builder = builder.event_recorder_factory(factory);
            }
            builder.run().context("TCP load test failed")?
        }

        DetectedProtocol::Udp => {
            let udp_payload = resolve_payload(&payload, &payload_hex, &payload_file)
                .context("invalid UDP payload")?;
            let targets = parse_socket_targets(&config.targets, "udp://")
                .context("invalid UDP target address")?;
            let expect_response = !no_response;

            eprintln!(
                "  protocol: UDP, {} byte payload, response: {expect_response}",
                udp_payload.len()
            );

            config.error_status_threshold = 0; // no HTTP status for UDP
            config.protocol = Some(netanvil_types::ProtocolConfig::Udp {
                payload_hex: encode_hex(&udp_payload),
                expect_response,
            });

            let gen_factory: netanvil_core::GenericGeneratorFactory<netanvil_udp::UdpRequestSpec> =
                if let Some(ref plugin_path) = plugin {
                    let ptype = detect_plugin_type(plugin_path, &plugin_type)?;
                    tracing::info!(
                        plugin = %plugin_path,
                        plugin_type = ?ptype,
                        "loaded UDP plugin generator"
                    );
                    build_udp_plugin_factory(plugin_path, ptype, &config.targets)?
                } else {
                    Box::new(move |_core_id| {
                        Box::new(netanvil_udp::SimpleUdpGenerator::new(
                            targets.clone(),
                            udp_payload.clone(),
                            expect_response,
                        ))
                    })
                };
            let trans_factory: netanvil_core::GenericTransformerFactory<
                netanvil_udp::UdpRequestSpec,
            > = Box::new(|_| Box::new(netanvil_udp::UdpNoopTransformer));

            let mut builder = GenericTestBuilder::new(
                config,
                move |_| netanvil_udp::UdpExecutor::with_timeout(request_timeout),
                gen_factory,
                trans_factory,
            )
            .on_progress(|update| {
                eprint!("\r{}", ProgressLine::new(update));
            });
            if let Some(factory) = event_recorder_factory {
                builder = builder.event_recorder_factory(factory);
            }
            builder.run().context("UDP load test failed")?
        }

        DetectedProtocol::Dns => {
            let servers = parse_socket_targets(&config.targets, "dns://")
                .context("invalid DNS target address")?;
            let domains: Vec<String> = dns_domains
                .as_deref()
                .unwrap_or("example.com")
                .split(',')
                .map(|s: &str| s.trim().to_string())
                .collect();
            let query_type = netanvil_dns::DnsQueryType::from_str_name(&dns_query_type)
                .ok_or_else(|| anyhow::anyhow!("unknown DNS query type: {dns_query_type}"))?;

            eprintln!(
                "  protocol: DNS, query_type: {:?}, domains: {}",
                query_type,
                domains.len()
            );

            config.error_status_threshold = 0;

            let gen_factory: netanvil_core::GenericGeneratorFactory<netanvil_dns::DnsRequestSpec> =
                if let Some(ref plugin_path) = plugin {
                    let ptype = detect_plugin_type(plugin_path, &plugin_type)?;
                    tracing::info!(
                        plugin = %plugin_path,
                        plugin_type = ?ptype,
                        "loaded DNS plugin generator"
                    );
                    build_dns_plugin_factory(plugin_path, ptype, &config.targets)?
                } else {
                    Box::new(move |_core_id| {
                        Box::new(netanvil_dns::SimpleDnsGenerator::new(
                            servers.clone(),
                            domains.clone(),
                            query_type,
                            true,
                        ))
                    })
                };
            let trans_factory: netanvil_core::GenericTransformerFactory<
                netanvil_dns::DnsRequestSpec,
            > = Box::new(|_| Box::new(netanvil_dns::DnsNoopTransformer));

            let mut builder = GenericTestBuilder::new(
                config,
                move |_| netanvil_dns::DnsExecutor::with_timeout(request_timeout),
                gen_factory,
                trans_factory,
            )
            .on_progress(|update| {
                eprint!("\r{}", ProgressLine::new(update));
            });
            if let Some(factory) = event_recorder_factory {
                builder = builder.event_recorder_factory(factory);
            }
            builder.run().context("DNS load test failed")?
        }

        DetectedProtocol::Redis => {
            let targets = parse_socket_targets(&config.targets, "redis://")
                .context("invalid Redis target address")?;

            eprintln!("  protocol: Redis, targets: {}", targets.len());

            config.error_status_threshold = 0; // Redis uses its own error model

            let gen_factory: netanvil_core::GenericGeneratorFactory<
                netanvil_redis::RedisRequestSpec,
            > = if let Some(ref plugin_path) = plugin {
                let ptype = detect_plugin_type(plugin_path, &plugin_type)?;
                tracing::info!(plugin = %plugin_path, plugin_type = ?ptype, "loaded Redis plugin");
                build_redis_plugin_factory(plugin_path, ptype, &config.targets)?
            } else {
                // Default: PING command
                Box::new(move |_core_id| {
                    Box::new(netanvil_redis::SimpleRedisGenerator::new(
                        targets.clone(),
                        "PING".into(),
                        vec![],
                    ))
                })
            };
            let trans_factory: netanvil_core::GenericTransformerFactory<
                netanvil_redis::RedisRequestSpec,
            > = Box::new(|_| Box::new(netanvil_redis::RedisNoopTransformer));

            let mut builder = GenericTestBuilder::new(
                config,
                move |_| netanvil_redis::RedisExecutor::new(request_timeout),
                gen_factory,
                trans_factory,
            )
            .on_progress(|update| {
                eprint!("\r{}", ProgressLine::new(update));
            });
            if let Some(factory) = event_recorder_factory {
                builder = builder.event_recorder_factory(factory);
            }
            builder.run().context("Redis load test failed")?
        }

        DetectedProtocol::Http => {
            // Build plugin generator factory if --plugin is specified
            let plugin_factory = if let Some(ref plugin_path) = plugin {
                let ptype = detect_plugin_type(plugin_path, &plugin_type)?;
                let factory = build_plugin_factory(plugin_path, ptype, &config.targets)?;
                tracing::info!(
                    plugin = %plugin_path,
                    plugin_type = ?ptype,
                    "loaded plugin generator"
                );
                Some(factory)
            } else {
                None
            };

            if health_sample_rate > 0.0 {
                // Verify kernel support before enabling health sampling.
                anyhow::ensure!(
                    netanvil_http::is_health_sampling_supported(),
                    "--health-sample-rate requires kernel >= 6.0 with io_uring URING_CMD support"
                );

                config.health_sample_rate = health_sample_rate;

                let tls_client = config.tls_client.clone();
                let make_executor =
                    move |health: Option<netanvil_types::HealthCounters>| -> HttpExecutor<Observe> {
                        let health = health.expect(
                            "engine must provide HealthCounters when health_sample_rate > 0",
                        );
                        let observe_config = ObserveConfig {
                            sample_rate: health_sample_rate,
                            worker_cpu: 0,
                            counters: health.affinity,
                            tcp_health: health.tcp_health,
                        };
                        let wrapper = Observe(SendObserveConfig::new(observe_config));
                        HttpExecutor::with_connection_config_wrapped(
                            tls_client.as_ref(),
                            bandwidth_bps,
                            request_timeout,
                            false,
                            None,
                            http_version,
                            wrapper,
                        )
                        .expect("executor configuration error")
                    };

                run_http_test(
                    config,
                    make_executor,
                    plugin_factory,
                    event_recorder_factory,
                    api_port,
                )?
            } else {
                let tls_client = config.tls_client.clone();
                let make_executor =
                    move |_health: Option<netanvil_types::HealthCounters>| -> HttpExecutor {
                        match &tls_client {
                            Some(tls_config) => HttpExecutor::with_tls_and_version(
                                tls_config,
                                bandwidth_bps,
                                request_timeout,
                                http_version,
                            )
                            .expect("TLS configuration error"),
                            None => HttpExecutor::with_http_version(
                                http_version,
                                bandwidth_bps,
                                request_timeout,
                            ),
                        }
                    };

                run_http_test(
                    config,
                    make_executor,
                    plugin_factory,
                    event_recorder_factory,
                    api_port,
                )?
            }
        }
    };

    eprintln!(); // newline after progress

    let output_format = output::OutputFormat::parse(&output).map_err(|e| anyhow::anyhow!(e))?;
    output::print_results(&result, output_format);

    Ok(())
}

/// Run an HTTP load test with a generic executor factory.
///
/// Encapsulates the builder pattern (with/without API port, plugins, events)
/// so the caller only needs to provide the executor factory.
fn run_http_test<E, F>(
    config: TestConfig,
    make_executor: F,
    plugin_factory: Option<
        Box<
            dyn Fn(
                    usize,
                ) -> Box<
                    dyn netanvil_types::RequestGenerator<Spec = netanvil_types::HttpRequestSpec>,
                > + Send,
        >,
    >,
    event_recorder_factory: Option<
        Box<dyn Fn(usize) -> Box<dyn netanvil_types::EventRecorder> + Send>,
    >,
    api_port: Option<u16>,
) -> Result<netanvil_core::TestResult>
where
    E: netanvil_types::RequestExecutor<Spec = netanvil_types::HttpRequestSpec> + 'static,
    F: Fn(Option<netanvil_types::HealthCounters>) -> E + Send + 'static,
{
    if let Some(port) = api_port {
        let shared_state = SharedState::new();
        let (ext_cmd_tx, ext_cmd_rx) = flume::unbounded();

        let server = ControlServer::new(port, shared_state.clone(), ext_cmd_tx)
            .context(format!("failed to start API server on port {port}"))?;
        let _server_handle = server.spawn();
        tracing::info!(port, "control API listening");

        let progress_state = shared_state.clone();
        let signal_state = shared_state.clone();
        let mut builder = TestBuilder::new(config, make_executor)
            .on_progress(move |update| {
                progress_state.update_from_progress(update);
                eprint!("\r{}", ProgressLine::new(update));
            })
            .external_commands(ext_cmd_rx)
            .pushed_signal_source(move || signal_state.drain_pushed_signals());

        if let Some(factory) = plugin_factory {
            builder = builder.generator_factory(factory);
        }
        if let Some(factory) = event_recorder_factory {
            builder = builder.event_recorder_factory(factory);
        }

        builder.run().context("load test failed")
    } else {
        let mut builder = TestBuilder::new(config, make_executor).on_progress(|update| {
            eprint!("\r{}", ProgressLine::new(update));
        });

        if let Some(factory) = plugin_factory {
            builder = builder.generator_factory(factory);
        }
        if let Some(factory) = event_recorder_factory {
            builder = builder.event_recorder_factory(factory);
        }

        builder.run().context("load test failed")
    }
}

/// Derive the protocol name for event logging from the config.
fn detect_protocol_name(config: &TestConfig) -> &'static str {
    match &config.protocol {
        Some(netanvil_types::ProtocolConfig::Tcp { .. }) => "tcp",
        Some(netanvil_types::ProtocolConfig::Udp { .. }) => "udp",
        Some(netanvil_types::ProtocolConfig::Dns { .. }) => "dns",
        Some(netanvil_types::ProtocolConfig::Redis { .. }) => "redis",
        None => "http",
    }
}
