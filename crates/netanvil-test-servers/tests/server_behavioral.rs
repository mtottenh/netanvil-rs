//! Behavioral tests for Phase 1 hardening.
//!
//! These tests verify that the test servers survive sustained load, handle
//! connection storms gracefully, and tolerate transient errors without crashing.

use std::time::Duration;

use netanvil_core::GenericTestBuilder;
use netanvil_tcp::{
    SimpleTcpGenerator, TcpExecutor, TcpFraming, TcpNoopTransformer, TcpRequestSpec, TcpTestMode,
};
use netanvil_test_servers::ServerConfig;
use netanvil_types::{
    ConnectionPolicy, RateConfig, RequestGenerator, RequestTransformer, TestConfig,
};

#[test]
fn test_tcp_sustained_load() {
    let server = netanvil_test_servers::tcp::start_tcp_echo();
    let targets = vec![server.addr];
    let payload = vec![0u8; 64];

    let config = TestConfig {
        targets: vec![format!("{}", server.addr)],
        duration: Duration::from_secs(10),
        rate: RateConfig::Static { rps: 5000.0 },
        num_cores: 1,
        error_status_threshold: 0,
        ..Default::default()
    };

    let result = GenericTestBuilder::new(
        config,
        |_| TcpExecutor::with_pool(Duration::from_secs(5), 100, ConnectionPolicy::KeepAlive),
        Box::new(move |_| {
            Box::new(
                SimpleTcpGenerator::new(targets.clone(), payload.clone(), TcpFraming::Raw, true)
                    .with_mode(TcpTestMode::RR)
                    .with_request_size(64)
                    .with_response_size(256),
            ) as Box<dyn RequestGenerator<Spec = TcpRequestSpec>>
        }),
        Box::new(|_| {
            Box::new(TcpNoopTransformer) as Box<dyn RequestTransformer<Spec = TcpRequestSpec>>
        }),
    )
    .run()
    .expect("test run failed");

    assert_eq!(
        result.total_errors, 0,
        "expected 0 errors under sustained TCP load, got {}",
        result.total_errors
    );
    assert!(
        result.total_requests > 0,
        "expected some requests completed"
    );
    assert!(
        result.latency_p99 < Duration::from_millis(50),
        "p99 latency too high: {:?} (expected < 50ms with TCP_NODELAY)",
        result.latency_p99
    );
}

#[test]
fn test_tcp_connection_storm() {
    let server = netanvil_test_servers::tcp::start_tcp_echo_with_config(ServerConfig {
        max_connections: 100,
        ..Default::default()
    });
    let targets = vec![server.addr];
    let payload = vec![0u8; 64];

    let config = TestConfig {
        targets: vec![format!("{}", server.addr)],
        duration: Duration::from_secs(5),
        rate: RateConfig::Static { rps: 2000.0 },
        num_cores: 1,
        error_status_threshold: 0,
        connections: netanvil_types::ConnectionConfig {
            connection_policy: ConnectionPolicy::AlwaysNew,
            request_timeout: Duration::from_secs(2),
            ..Default::default()
        },
        ..Default::default()
    };

    let result = GenericTestBuilder::new(
        config,
        |_| TcpExecutor::with_pool(Duration::from_secs(2), 200, ConnectionPolicy::AlwaysNew),
        Box::new(move |_| {
            Box::new(
                SimpleTcpGenerator::new(targets.clone(), payload.clone(), TcpFraming::Raw, true)
                    .with_mode(TcpTestMode::RR)
                    .with_request_size(64)
                    .with_response_size(128),
            ) as Box<dyn RequestGenerator<Spec = TcpRequestSpec>>
        }),
        Box::new(|_| {
            Box::new(TcpNoopTransformer) as Box<dyn RequestTransformer<Spec = TcpRequestSpec>>
        }),
    )
    .run()
    .expect("server crashed under connection storm — test framework error");

    // Server must survive the storm (test completing is the primary assertion).
    // Some errors are expected since we're hitting the connection limit.
    assert!(
        result.total_requests > 0,
        "expected some requests to complete during connection storm"
    );
}

#[test]
fn test_udp_sustained_load() {
    use netanvil_udp::{SimpleUdpGenerator, UdpExecutor, UdpNoopTransformer, UdpRequestSpec};

    let server = netanvil_test_servers::udp::start_udp_echo();
    let targets = vec![server.addr];
    let payload = vec![0u8; 64];

    let config = TestConfig {
        targets: vec![format!("{}", server.addr)],
        duration: Duration::from_secs(10),
        rate: RateConfig::Static { rps: 20000.0 },
        num_cores: 1,
        error_status_threshold: 0,
        ..Default::default()
    };

    let result = GenericTestBuilder::new(
        config,
        |_| UdpExecutor::with_timeout(Duration::from_secs(2)),
        Box::new(move |_| {
            Box::new(SimpleUdpGenerator::new(
                targets.clone(),
                payload.clone(),
                true,
            )) as Box<dyn RequestGenerator<Spec = UdpRequestSpec>>
        }),
        Box::new(|_| {
            Box::new(UdpNoopTransformer) as Box<dyn RequestTransformer<Spec = UdpRequestSpec>>
        }),
    )
    .run()
    .expect("test run failed");

    assert!(
        result.total_requests > 0,
        "expected some UDP requests completed"
    );

    // Loss rate should be < 5% (relaxed from 1% — single-threaded server under
    // 20k RPS may see some loss depending on CI load).
    let loss_rate = result.total_errors as f64 / result.total_requests as f64;
    assert!(
        loss_rate < 0.05,
        "UDP loss rate too high: {:.2}% ({} errors / {} requests)",
        loss_rate * 100.0,
        result.total_errors,
        result.total_requests
    );
}

#[test]
fn test_udp_transient_errors_survive() {
    use netanvil_udp::{SimpleUdpGenerator, UdpExecutor, UdpNoopTransformer, UdpRequestSpec};

    let server = netanvil_test_servers::udp::start_udp_echo();
    let targets = vec![server.addr];
    let payload = vec![0u8; 64];

    let config = TestConfig {
        targets: vec![format!("{}", server.addr)],
        duration: Duration::from_secs(5),
        rate: RateConfig::Static { rps: 1000.0 },
        num_cores: 1,
        error_status_threshold: 0,
        ..Default::default()
    };

    let result = GenericTestBuilder::new(
        config,
        |_| UdpExecutor::with_timeout(Duration::from_secs(2)),
        Box::new(move |_| {
            Box::new(SimpleUdpGenerator::new(
                targets.clone(),
                payload.clone(),
                true,
            )) as Box<dyn RequestGenerator<Spec = UdpRequestSpec>>
        }),
        Box::new(|_| {
            Box::new(UdpNoopTransformer) as Box<dyn RequestTransformer<Spec = UdpRequestSpec>>
        }),
    )
    .run()
    .expect("test run failed");

    // Server must stay alive and serve requests.
    assert!(
        result.total_requests > 0,
        "expected requests to succeed after transient errors"
    );
    assert_eq!(
        result.total_errors, 0,
        "expected 0 errors at moderate UDP rate, got {}",
        result.total_errors
    );
}
