use std::time::Duration;

use netanvil_core::GenericTestBuilder;
use netanvil_tcp::{
    SimpleTcpGenerator, TcpExecutor, TcpFraming, TcpNoopTransformer, TcpRequestSpec, TcpTestMode,
};
use netanvil_types::{
    ConnectionPolicy, RateConfig, RequestGenerator, RequestTransformer, TestConfig,
};

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
        |_| TcpExecutor::with_timeout(Duration::from_secs(5)),
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
        |_| TcpExecutor::with_timeout(Duration::from_secs(5)),
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
        |_| TcpExecutor::with_timeout(Duration::from_secs(5)),
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
        |_| TcpExecutor::with_timeout(Duration::from_secs(2)),
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

#[test]
fn test_tcp_rr_mode() {
    let server = netanvil_test_servers::tcp::start_tcp_echo();
    let targets = vec![server.addr];
    let payload = vec![0u8; 64]; // 64-byte request

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
        |_| TcpExecutor::with_pool(Duration::from_secs(5), 10, ConnectionPolicy::KeepAlive),
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
    .expect("test run failed");

    assert!(
        result.total_requests > 0,
        "expected some RR requests, got 0"
    );
    assert_eq!(
        result.total_errors, 0,
        "expected 0 errors in RR mode, got {}",
        result.total_errors
    );
    // RR mode should have received response bytes
    assert!(
        result.total_bytes_received > 0,
        "expected received bytes in RR mode"
    );
}

#[test]
fn test_tcp_sink_mode() {
    let server = netanvil_test_servers::tcp::start_tcp_echo();
    let targets = vec![server.addr];
    let chunk = vec![0xAAu8; 1024]; // 1KB chunks

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
        |_| TcpExecutor::with_pool(Duration::from_secs(5), 10, ConnectionPolicy::KeepAlive),
        Box::new(move |_| {
            Box::new(
                SimpleTcpGenerator::new(targets.clone(), chunk.clone(), TcpFraming::Raw, false)
                    .with_mode(TcpTestMode::Sink)
                    .with_request_size(1024)
                    .with_response_size(0),
            ) as Box<dyn RequestGenerator<Spec = TcpRequestSpec>>
        }),
        Box::new(|_| {
            Box::new(TcpNoopTransformer) as Box<dyn RequestTransformer<Spec = TcpRequestSpec>>
        }),
    )
    .run()
    .expect("test run failed");

    assert!(
        result.total_requests > 0,
        "expected some SINK requests, got 0"
    );
    assert_eq!(
        result.total_errors, 0,
        "expected 0 errors in SINK mode, got {}",
        result.total_errors
    );
}

#[test]
fn test_tcp_source_mode() {
    let server = netanvil_test_servers::tcp::start_tcp_echo();
    let targets = vec![server.addr];
    let payload = Vec::new(); // no client payload for source mode

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
        |_| TcpExecutor::with_pool(Duration::from_secs(5), 10, ConnectionPolicy::KeepAlive),
        Box::new(move |_| {
            Box::new(
                SimpleTcpGenerator::new(targets.clone(), payload.clone(), TcpFraming::Raw, true)
                    .with_mode(TcpTestMode::Source)
                    .with_request_size(0)
                    .with_response_size(1024),
            ) as Box<dyn RequestGenerator<Spec = TcpRequestSpec>>
        }),
        Box::new(|_| {
            Box::new(TcpNoopTransformer) as Box<dyn RequestTransformer<Spec = TcpRequestSpec>>
        }),
    )
    .run()
    .expect("test run failed");

    assert!(
        result.total_requests > 0,
        "expected some SOURCE requests, got 0"
    );
    assert_eq!(
        result.total_errors, 0,
        "expected 0 errors in SOURCE mode, got {}",
        result.total_errors
    );
    assert!(
        result.total_bytes_received > 0,
        "expected received bytes in SOURCE mode"
    );
}

#[test]
fn test_tcp_pool_connection_reuse() {
    // With KeepAlive policy, connections should be reused.
    // We verify by checking that total_requests > 0 with 0 errors,
    // which means the pool is working (requests go through even though
    // new connections have a TCP handshake cost).
    let server = netanvil_test_servers::tcp::start_tcp_echo();
    let targets = vec![server.addr];
    let payload = b"HELLO".to_vec();

    let config = TestConfig {
        targets: vec![format!("{}", server.addr)],
        duration: Duration::from_secs(2),
        rate: RateConfig::Static { rps: 200.0 },
        num_cores: 1,
        error_status_threshold: 0,
        ..Default::default()
    };

    let result = GenericTestBuilder::new(
        config,
        |_| TcpExecutor::with_pool(Duration::from_secs(5), 10, ConnectionPolicy::KeepAlive),
        Box::new(move |_| {
            Box::new(
                SimpleTcpGenerator::new(targets.clone(), payload.clone(), TcpFraming::Raw, true)
                    .with_mode(TcpTestMode::RR)
                    .with_request_size(5)
                    .with_response_size(5),
            ) as Box<dyn RequestGenerator<Spec = TcpRequestSpec>>
        }),
        Box::new(|_| {
            Box::new(TcpNoopTransformer) as Box<dyn RequestTransformer<Spec = TcpRequestSpec>>
        }),
    )
    .run()
    .expect("test run failed");

    // At 200 RPS for 2s = ~400 requests. With pooling, most should succeed.
    assert!(
        result.total_requests > 100,
        "expected >100 pooled RR requests, got {}",
        result.total_requests
    );
    assert_eq!(
        result.total_errors, 0,
        "expected 0 errors with pooled connections, got {}",
        result.total_errors
    );
}
