use std::time::Duration;

use netanvil_core::GenericTestBuilder;
use netanvil_tcp::{SimpleTcpGenerator, TcpExecutor, TcpFraming, TcpNoopTransformer, TcpRequestSpec};
use netanvil_types::{RateConfig, RequestGenerator, RequestTransformer, TestConfig};

#[test]
fn test_tcp_echo_raw() {
    let server = netanvil_test_servers::tcp::start_tcp_echo();
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
        || TcpExecutor::with_timeout(Duration::from_secs(5)),
        Box::new(move |_| {
            Box::new(SimpleTcpGenerator::new(
                targets.clone(),
                payload.clone(),
                TcpFraming::Raw,
                true,
            )) as Box<dyn RequestGenerator<Spec = TcpRequestSpec>>
        }),
        Box::new(|_| {
            Box::new(TcpNoopTransformer) as Box<dyn RequestTransformer<Spec = TcpRequestSpec>>
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
fn test_tcp_echo_delimiter() {
    let server = netanvil_test_servers::tcp::start_tcp_echo();
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
        || TcpExecutor::with_timeout(Duration::from_secs(5)),
        Box::new(move |_| {
            Box::new(SimpleTcpGenerator::new(
                targets.clone(),
                payload.clone(),
                TcpFraming::Delimiter(b"\r\n".to_vec()),
                true,
            )) as Box<dyn RequestGenerator<Spec = TcpRequestSpec>>
        }),
        Box::new(|_| {
            Box::new(TcpNoopTransformer) as Box<dyn RequestTransformer<Spec = TcpRequestSpec>>
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
fn test_tcp_fire_and_forget() {
    let server = netanvil_test_servers::tcp::start_tcp_echo();
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
        || TcpExecutor::with_timeout(Duration::from_secs(5)),
        Box::new(move |_| {
            Box::new(SimpleTcpGenerator::new(
                targets.clone(),
                payload.clone(),
                TcpFraming::Raw,
                false, // expect_response = false
            )) as Box<dyn RequestGenerator<Spec = TcpRequestSpec>>
        }),
        Box::new(|_| {
            Box::new(TcpNoopTransformer) as Box<dyn RequestTransformer<Spec = TcpRequestSpec>>
        }),
    )
    .run()
    .expect("test run failed");

    assert!(result.total_requests > 0, "expected some requests, got 0");
}

#[test]
fn test_tcp_connection_refused() {
    // Target a port where nothing is listening
    let targets = vec!["127.0.0.1:1".parse().unwrap()];
    let payload = b"PING".to_vec();

    let config = TestConfig {
        targets: vec!["127.0.0.1:1".to_string()],
        duration: Duration::from_secs(2),
        rate: RateConfig::Static { rps: 10.0 },
        num_cores: 1,
        error_status_threshold: 0,
        ..Default::default()
    };

    let result = GenericTestBuilder::new(
        config,
        || TcpExecutor::with_timeout(Duration::from_secs(2)),
        Box::new(move |_| {
            Box::new(SimpleTcpGenerator::new(
                targets.clone(),
                payload.clone(),
                TcpFraming::Raw,
                true,
            )) as Box<dyn RequestGenerator<Spec = TcpRequestSpec>>
        }),
        Box::new(|_| {
            Box::new(TcpNoopTransformer) as Box<dyn RequestTransformer<Spec = TcpRequestSpec>>
        }),
    )
    .run()
    .expect("test run failed");

    assert!(
        result.total_errors > 0,
        "expected some errors for connection refused, got 0"
    );
}
