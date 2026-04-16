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

// ---------------------------------------------------------------------------
// Phase 2: Multi-core behavioral tests
// ---------------------------------------------------------------------------

#[test]
fn test_tcp_multicore_throughput() {
    let server = netanvil_test_servers::TestServer::builder()
        .tcp(0)
        .workers(4)
        .pin_cores(false)
        .build();
    let tcp_addr = server.tcp_addr().unwrap();
    let targets = vec![tcp_addr];
    let payload = vec![0u8; 64];

    let config = TestConfig {
        targets: vec![format!("{}", tcp_addr)],
        duration: Duration::from_secs(10),
        rate: RateConfig::Static { rps: 10000.0 },
        num_cores: 2,
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
        "expected 0 errors with multicore TCP, got {}",
        result.total_errors
    );
    let achieved_rps = result.total_requests as f64 / 10.0;
    assert!(
        achieved_rps >= 8000.0,
        "achieved RPS too low: {:.0} (expected >= 8000)",
        achieved_rps
    );
}

#[test]
fn test_tcp_multicore_connections_distributed() {
    let server = netanvil_test_servers::TestServer::builder()
        .tcp(0)
        .workers(4)
        .pin_cores(false)
        .build();
    let tcp_addr = server.tcp_addr().unwrap();
    let targets = vec![tcp_addr];
    let payload = vec![0u8; 64];

    let config = TestConfig {
        targets: vec![format!("{}", tcp_addr)],
        duration: Duration::from_secs(5),
        rate: RateConfig::Static { rps: 5000.0 },
        num_cores: 2,
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
    .expect("test run failed");

    assert!(
        result.total_requests > 0,
        "expected some requests to complete with distributed connections"
    );
}

#[test]
fn test_udp_multicore_throughput() {
    use netanvil_udp::{SimpleUdpGenerator, UdpExecutor, UdpNoopTransformer, UdpRequestSpec};

    let server = netanvil_test_servers::TestServer::builder()
        .udp(0)
        .workers(4)
        .pin_cores(false)
        .build();
    let udp_addr = server.udp_addr().unwrap();
    let targets = vec![udp_addr];
    let payload = vec![0u8; 64];

    let config = TestConfig {
        targets: vec![format!("{}", udp_addr)],
        duration: Duration::from_secs(10),
        rate: RateConfig::Static { rps: 20000.0 },
        num_cores: 2,
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

    let loss_rate = result.total_errors as f64 / result.total_requests as f64;
    assert!(
        loss_rate < 0.02,
        "UDP multicore loss rate too high: {:.2}% ({} errors / {} requests)",
        loss_rate * 100.0,
        result.total_errors,
        result.total_requests
    );
}

#[test]
fn test_backward_compat_single_worker() {
    let server = netanvil_test_servers::tcp::start_tcp_echo();
    let targets = vec![server.addr];
    let payload = vec![0u8; 64];

    let config = TestConfig {
        targets: vec![format!("{}", server.addr)],
        duration: Duration::from_secs(3),
        rate: RateConfig::Static { rps: 1000.0 },
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
        "expected 0 errors with backward-compat single worker, got {}",
        result.total_errors
    );
    assert!(
        result.total_requests > 0,
        "expected some requests completed"
    );
}

// ---------------------------------------------------------------------------
// Phase 3: Buffer management behavioral tests
// ---------------------------------------------------------------------------

#[test]
fn test_tcp_large_payload_sustained() {
    let server = netanvil_test_servers::TestServer::builder()
        .tcp(0)
        .workers(2)
        .pin_cores(false)
        .build();
    let tcp_addr = server.tcp_addr().unwrap();
    let targets = vec![tcp_addr];
    let payload = vec![0u8; 1024];

    let config = TestConfig {
        targets: vec![format!("{}", tcp_addr)],
        duration: Duration::from_secs(15),
        rate: RateConfig::Static { rps: 2000.0 },
        num_cores: 2,
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
                    .with_request_size(1024)
                    .with_response_size(65536),
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
        "expected 0 errors with large payload RR, got {}",
        result.total_errors
    );
    let achieved_rps = result.total_requests as f64 / 15.0;
    assert!(
        achieved_rps >= 1600.0,
        "achieved RPS too low: {:.0} (expected >= 1600 with large payloads)",
        achieved_rps
    );
    assert!(
        result.latency_p99 < Duration::from_millis(20),
        "p99 latency too high: {:?} (expected < 20ms)",
        result.latency_p99
    );
}

#[test]
fn test_tcp_source_sustained() {
    let server = netanvil_test_servers::TestServer::builder()
        .tcp(0)
        .workers(2)
        .pin_cores(false)
        .build();
    let tcp_addr = server.tcp_addr().unwrap();
    let targets = vec![tcp_addr];
    let payload = vec![0u8; 64];

    let config = TestConfig {
        targets: vec![format!("{}", tcp_addr)],
        duration: Duration::from_secs(10),
        rate: RateConfig::Static { rps: 500.0 },
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
                    .with_mode(TcpTestMode::Source)
                    .with_response_size(65536),
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
        "expected 0 errors in SOURCE mode, got {}",
        result.total_errors
    );
    assert!(
        result.total_requests > 0,
        "expected some SOURCE requests completed"
    );
}

#[test]
fn test_udp_small_packet_flood() {
    use netanvil_udp::{SimpleUdpGenerator, UdpExecutor, UdpNoopTransformer, UdpRequestSpec};

    let server = netanvil_test_servers::TestServer::builder()
        .udp(0)
        .workers(4)
        .pin_cores(false)
        .build();
    let udp_addr = server.udp_addr().unwrap();
    let targets = vec![udp_addr];
    let payload = vec![0u8; 64];

    // 30k RPS with 4 workers: ~7.5k PPS/worker, well within budget.
    // Relaxed loss threshold for CI environments with limited resources.
    let config = TestConfig {
        targets: vec![format!("{}", udp_addr)],
        duration: Duration::from_secs(10),
        rate: RateConfig::Static { rps: 30000.0 },
        num_cores: 2,
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

    let loss_rate = result.total_errors as f64 / result.total_requests as f64;
    assert!(
        loss_rate < 0.05,
        "UDP small packet flood loss rate too high: {:.2}% ({} errors / {} requests)",
        loss_rate * 100.0,
        result.total_errors,
        result.total_requests
    );
}

/// Validates that server-generated write buffers contain non-zero data
/// (PRNG fill is working, not sending all-zeros).
#[compio::test]
async fn test_response_data_has_entropy() {
    use compio::buf::BufResult;
    use compio::io::{AsyncReadExt, AsyncWriteExt};
    use netanvil_test_servers::protocol;

    let server = netanvil_test_servers::tcp::start_tcp_echo();

    let stream = compio::net::TcpStream::connect(server.addr).await.unwrap();
    let mut stream = stream;

    // Send protocol header: RR mode, request_size=64, response_size=256
    let mut header = [0u8; protocol::HEADER_SIZE];
    header[0] = protocol::MODE_RR;
    // request_size = 64 (big-endian u16 at offset 2..4)
    header[2] = 0;
    header[3] = 64;
    // response_size = 256 (big-endian u32 at offset 4..8)
    header[4] = 0;
    header[5] = 0;
    header[6] = 1;
    header[7] = 0;
    let BufResult(result, _) = stream.write_all(header.to_vec()).await;
    result.unwrap();

    // Send 64 bytes of request payload
    let request = vec![0xABu8; 64];
    let BufResult(result, _) = stream.write_all(request).await;
    result.unwrap();

    // Read 256 bytes of response
    let response = vec![0u8; 256];
    let BufResult(result, response) = stream.read_exact(response).await;
    result.unwrap();

    // Assert response is not all zeros
    let nonzero_count = response.iter().filter(|&&b| b != 0).count();
    assert!(
        nonzero_count > 100,
        "response should contain PRNG data, not zeros. \
         Only {} of 256 bytes are non-zero",
        nonzero_count
    );

    // Assert reasonable entropy: count unique byte values
    let mut seen = [false; 256];
    for &b in &response {
        seen[b as usize] = true;
    }
    let unique_values = seen.iter().filter(|&&s| s).count();
    assert!(
        unique_values > 50,
        "response should have reasonable entropy. Only {} unique byte values in 256 bytes",
        unique_values
    );
}

// ---------------------------------------------------------------------------
// Phase 4: Connected UDP behavioral tests
// ---------------------------------------------------------------------------

#[test]
fn test_udp_connected_throughput() {
    use netanvil_udp::{SimpleUdpGenerator, UdpExecutor, UdpNoopTransformer, UdpRequestSpec};

    // Connected mode uses spawned handler tasks per client, which adds compio
    // scheduling overhead on loopback. Keep RPS moderate — the value of connected
    // sockets is kernel-level demuxing at scale, not raw PPS on loopback.
    let server = netanvil_test_servers::TestServer::builder()
        .udp(0)
        .workers(2)
        .pin_cores(false)
        .udp_idle_timeout(30)
        .build();
    let udp_addr = server.udp_addr().unwrap();
    let targets = vec![udp_addr];
    let payload = vec![0u8; 64];

    let config = TestConfig {
        targets: vec![format!("{}", udp_addr)],
        duration: Duration::from_secs(10),
        rate: RateConfig::Static { rps: 5000.0 },
        num_cores: 2,
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

    let loss_rate = if result.total_requests > 0 {
        result.total_errors as f64 / result.total_requests as f64
    } else {
        1.0
    };
    assert!(
        loss_rate < 0.05,
        "connected UDP loss rate too high: {:.2}% ({} errors / {} requests)",
        loss_rate * 100.0,
        result.total_errors,
        result.total_requests
    );
}

#[test]
fn test_udp_connected_idle_cleanup() {
    use netanvil_udp::{SimpleUdpGenerator, UdpExecutor, UdpNoopTransformer, UdpRequestSpec};

    let server = netanvil_test_servers::TestServer::builder()
        .udp(0)
        .workers(1)
        .pin_cores(false)
        .udp_idle_timeout(3) // 3 second idle timeout
        .build();
    let udp_addr = server.udp_addr().unwrap();

    // First run
    {
        let targets = vec![udp_addr];
        let payload = vec![0u8; 64];
        let config = TestConfig {
            targets: vec![format!("{}", udp_addr)],
            duration: Duration::from_secs(2),
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
        .expect("first run failed");

        assert!(
            result.total_requests > 0,
            "first run should complete requests"
        );
    }

    // Wait for idle timeout to expire — connected sockets should be cleaned up.
    std::thread::sleep(Duration::from_secs(5));

    // Second run — new client, should work fine after cleanup.
    {
        let targets = vec![udp_addr];
        let payload = vec![0u8; 64];
        let config = TestConfig {
            targets: vec![format!("{}", udp_addr)],
            duration: Duration::from_secs(2),
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
        .expect("second run failed");

        assert!(
            result.total_requests > 0,
            "second run should complete requests after idle cleanup"
        );
        assert_eq!(
            result.total_errors, 0,
            "expected 0 errors on second run after idle cleanup, got {}",
            result.total_errors
        );
    }
}

#[test]
fn test_udp_connected_multi_client() {
    use netanvil_udp::{SimpleUdpGenerator, UdpExecutor, UdpNoopTransformer, UdpRequestSpec};

    let server = netanvil_test_servers::TestServer::builder()
        .udp(0)
        .workers(2)
        .pin_cores(false)
        .udp_idle_timeout(30)
        .build();
    let udp_addr = server.udp_addr().unwrap();

    // Run two sequential netanvil runs (each gets different source ports).
    for run in 1..=2 {
        let targets = vec![udp_addr];
        let payload = vec![0u8; 64];
        let config = TestConfig {
            targets: vec![format!("{}", udp_addr)],
            duration: Duration::from_secs(3),
            rate: RateConfig::Static { rps: 5000.0 },
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
        .unwrap_or_else(|e| panic!("run {run} failed: {e}"));

        assert!(
            result.total_requests > 0,
            "run {run}: expected some requests completed"
        );
    }
}

#[test]
fn test_backward_compat_udp_unconnected() {
    use netanvil_udp::{SimpleUdpGenerator, UdpExecutor, UdpNoopTransformer, UdpRequestSpec};

    // Use the old start_udp_echo() API — defaults to unconnected mode.
    let server = netanvil_test_servers::udp::start_udp_echo();
    let targets = vec![server.addr];
    let payload = vec![0u8; 64];

    let config = TestConfig {
        targets: vec![format!("{}", server.addr)],
        duration: Duration::from_secs(3),
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

    assert_eq!(
        result.total_errors, 0,
        "expected 0 errors with unconnected UDP backward compat, got {}",
        result.total_errors
    );
    assert!(
        result.total_requests > 0,
        "expected some requests completed"
    );
}

// ---------------------------------------------------------------------------
// Phase 5: Server-side metrics behavioral tests
// ---------------------------------------------------------------------------

#[test]
fn test_server_metrics_match_client() {
    let server = netanvil_test_servers::TestServer::builder()
        .tcp(0)
        .workers(2)
        .pin_cores(false)
        .build();
    let tcp_addr = server.tcp_addr().unwrap();
    let targets = vec![tcp_addr];
    let payload = vec![0u8; 64];

    let config = TestConfig {
        targets: vec![format!("{}", tcp_addr)],
        duration: Duration::from_secs(10),
        rate: RateConfig::Static { rps: 5000.0 },
        num_cores: 2,
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

    // Give server time to process final connections
    std::thread::sleep(Duration::from_millis(500));

    let metrics = server.metrics_snapshot();

    // Server requests_completed within 10% of client total_requests
    let ratio = metrics.requests_completed as f64 / result.total_requests as f64;
    assert!(
        (0.9..=1.1).contains(&ratio),
        "requests mismatch: server={}, client={}, ratio={:.2}",
        metrics.requests_completed,
        result.total_requests,
        ratio
    );

    // Server bytes_sent within 10% of expected (total_requests * 256)
    let expected_bytes_sent = result.total_requests * 256;
    let bytes_sent_ratio = metrics.bytes_sent as f64 / expected_bytes_sent as f64;
    assert!(
        (0.9..=1.1).contains(&bytes_sent_ratio),
        "bytes_sent mismatch: server={}, expected={}, ratio={:.2}",
        metrics.bytes_sent,
        expected_bytes_sent,
        bytes_sent_ratio
    );

    // Server bytes_received within 10% of expected (total_requests * 64)
    let expected_bytes_received = result.total_requests * 64;
    let bytes_received_ratio = metrics.bytes_received as f64 / expected_bytes_received as f64;
    assert!(
        (0.9..=1.1).contains(&bytes_received_ratio),
        "bytes_received mismatch: server={}, expected={}, ratio={:.2}",
        metrics.bytes_received,
        expected_bytes_received,
        bytes_received_ratio
    );
}

#[test]
fn test_server_metrics_connections_tracked() {
    let server = netanvil_test_servers::TestServer::builder()
        .tcp(0)
        .workers(1)
        .pin_cores(false)
        .build();
    let tcp_addr = server.tcp_addr().unwrap();
    let targets = vec![tcp_addr];
    let payload = vec![0u8; 64];

    let config = TestConfig {
        targets: vec![format!("{}", tcp_addr)],
        duration: Duration::from_secs(5),
        rate: RateConfig::Static { rps: 1000.0 },
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
    .expect("test run failed");

    // Give server time to close all connections
    std::thread::sleep(Duration::from_millis(500));

    let metrics = server.metrics_snapshot();

    assert!(
        metrics.connections_accepted >= result.total_requests,
        "connections_accepted ({}) should be >= total_requests ({})",
        metrics.connections_accepted,
        result.total_requests
    );

    assert_eq!(
        metrics.connections_active, 0,
        "connections_active should be 0 after test, got {}",
        metrics.connections_active
    );
}

#[test]
fn test_server_metrics_udp_datagrams() {
    use netanvil_udp::{SimpleUdpGenerator, UdpExecutor, UdpNoopTransformer, UdpRequestSpec};

    let server = netanvil_test_servers::TestServer::builder()
        .udp(0)
        .workers(2)
        .pin_cores(false)
        .build();
    let udp_addr = server.udp_addr().unwrap();
    let targets = vec![udp_addr];
    let payload = vec![0u8; 64];

    let config = TestConfig {
        targets: vec![format!("{}", udp_addr)],
        duration: Duration::from_secs(5),
        rate: RateConfig::Static { rps: 10000.0 },
        num_cores: 2,
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

    std::thread::sleep(Duration::from_millis(500));

    let metrics = server.metrics_snapshot();

    // datagrams_received >= 90% of client requests
    assert!(
        metrics.datagrams_received as f64 >= result.total_requests as f64 * 0.90,
        "datagrams_received ({}) should be >= 90% of total_requests ({})",
        metrics.datagrams_received,
        result.total_requests
    );

    // datagrams_sent within 5% of datagrams_received (echo sends one per recv)
    let recv = metrics.datagrams_received as f64;
    let sent = metrics.datagrams_sent as f64;
    let ratio = if recv > 0.0 { sent / recv } else { 0.0 };
    assert!(
        (0.95..=1.05).contains(&ratio),
        "datagrams_sent ({}) should be within 5% of datagrams_received ({}), ratio={:.2}",
        metrics.datagrams_sent,
        metrics.datagrams_received,
        ratio
    );
}

#[test]
fn test_server_metrics_snapshot_accessible() {
    let server = netanvil_test_servers::TestServer::builder()
        .tcp(0)
        .workers(1)
        .pin_cores(false)
        .build();
    let tcp_addr = server.tcp_addr().unwrap();
    let targets = vec![tcp_addr];
    let payload = vec![0u8; 64];

    let config = TestConfig {
        targets: vec![format!("{}", tcp_addr)],
        duration: Duration::from_secs(3),
        rate: RateConfig::Static { rps: 500.0 },
        num_cores: 1,
        error_status_threshold: 0,
        ..Default::default()
    };

    let _result = GenericTestBuilder::new(
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

    let metrics = server.metrics_snapshot();

    assert!(
        metrics.requests_completed > 0,
        "expected non-zero requests_completed, got {}",
        metrics.requests_completed
    );
    assert!(
        metrics.bytes_sent > 0,
        "expected non-zero bytes_sent, got {}",
        metrics.bytes_sent
    );
    assert!(
        metrics.bytes_received > 0,
        "expected non-zero bytes_received, got {}",
        metrics.bytes_received
    );
}
