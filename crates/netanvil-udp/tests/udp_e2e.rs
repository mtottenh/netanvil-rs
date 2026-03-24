use std::time::Duration;

use netanvil_core::GenericTestBuilder;
use netanvil_types::{RateConfig, RequestGenerator, RequestTransformer, TestConfig};
use netanvil_udp::{SimpleUdpGenerator, UdpExecutor, UdpNoopTransformer, UdpRequestSpec};

#[test]
fn test_udp_echo() {
    let server = netanvil_test_servers::udp::start_udp_echo();
    let targets = vec![server.addr];
    let payload = b"PING".to_vec();

    let config = TestConfig {
        targets: vec![format!("{}", server.addr)],
        duration: Duration::from_secs(2),
        rate: RateConfig::Static { rps: 50.0 },
        num_cores: 1,
        error_status_threshold: 0,
        ..Default::default()
    };

    let result = GenericTestBuilder::new(
        config,
        |_| UdpExecutor::with_timeout(Duration::from_secs(5)),
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

    assert!(result.total_requests > 0, "expected some requests, got 0");
    assert_eq!(
        result.total_errors, 0,
        "expected 0 errors, got {}",
        result.total_errors
    );
}

#[test]
fn test_udp_fire_and_forget() {
    let server = netanvil_test_servers::udp::start_udp_echo();
    let targets = vec![server.addr];
    let payload = b"FIRE".to_vec();

    let config = TestConfig {
        targets: vec![format!("{}", server.addr)],
        duration: Duration::from_secs(2),
        rate: RateConfig::Static { rps: 100.0 },
        num_cores: 1,
        error_status_threshold: 0,
        ..Default::default()
    };

    let result = GenericTestBuilder::new(
        config,
        |_| UdpExecutor::with_timeout(Duration::from_secs(5)),
        Box::new(move |_| {
            Box::new(SimpleUdpGenerator::new(
                targets.clone(),
                payload.clone(),
                false, // no response expected
            )) as Box<dyn RequestGenerator<Spec = UdpRequestSpec>>
        }),
        Box::new(|_| {
            Box::new(UdpNoopTransformer) as Box<dyn RequestTransformer<Spec = UdpRequestSpec>>
        }),
    )
    .run()
    .expect("test run failed");

    assert!(result.total_requests > 0, "expected some requests, got 0");
}

#[test]
fn test_udp_no_server_timeout() {
    // Target a port with no server — expect timeouts
    let targets = vec!["127.0.0.1:1".parse().unwrap()];
    let payload = b"TIMEOUT".to_vec();

    let config = TestConfig {
        targets: vec!["127.0.0.1:1".to_string()],
        duration: Duration::from_secs(2),
        rate: RateConfig::Static { rps: 10.0 },
        num_cores: 1,
        error_status_threshold: 0,
        connections: netanvil_types::ConnectionConfig {
            request_timeout: Duration::from_millis(200),
            ..Default::default()
        },
        ..Default::default()
    };

    let result = GenericTestBuilder::new(
        config,
        |_| UdpExecutor::with_timeout(Duration::from_millis(200)),
        Box::new(move |_| {
            Box::new(SimpleUdpGenerator::new(
                targets.clone(),
                payload.clone(),
                true, // expect response (will timeout)
            )) as Box<dyn RequestGenerator<Spec = UdpRequestSpec>>
        }),
        Box::new(|_| {
            Box::new(UdpNoopTransformer) as Box<dyn RequestTransformer<Spec = UdpRequestSpec>>
        }),
    )
    .run()
    .expect("test run failed");

    // All requests should have errors (timeout or no response)
    assert!(
        result.total_errors > 0,
        "expected errors from missing server, got 0"
    );
}
