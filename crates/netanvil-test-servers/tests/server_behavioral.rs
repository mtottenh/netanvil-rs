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
#[serial_test::serial(udp)]
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
#[serial_test::serial(udp)]
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
#[serial_test::serial(udp)]
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
#[serial_test::serial(udp)]
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

    // Send v2 protocol header: RR mode, request_size=64, response_size=256
    let mut header = vec![0u8; protocol::HEADER_MIN];
    header[0] = protocol::VERSION_V2;
    header[1] = protocol::HEADER_MIN as u8;
    header[2] = protocol::MODE_RR;
    header[3] = 0; // no flags
                   // request_size = 64 (big-endian u16 at offset 4..6)
    header[4] = 0;
    header[5] = 64;
    // response_size = 256 (big-endian u32 at offset 6..10)
    header[6] = 0;
    header[7] = 0;
    header[8] = 1;
    header[9] = 0;
    let BufResult(result, _) = stream.write_all(header).await;
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
#[serial_test::serial(udp)]
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
#[serial_test::serial(udp)]
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
#[serial_test::serial(udp)]
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
#[serial_test::serial(udp)]
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
#[serial_test::serial(udp)]
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

// ---------------------------------------------------------------------------
// Phase 6A: v2 Protocol Header + CRR Mode + TCP Injection
// ---------------------------------------------------------------------------

#[test]
fn test_crr_mode() {
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
        rate: RateConfig::Static { rps: 3000.0 },
        num_cores: 2,
        error_status_threshold: 0,
        connections: netanvil_types::ConnectionConfig {
            connection_policy: ConnectionPolicy::AlwaysNew,
            request_timeout: Duration::from_secs(5),
            ..Default::default()
        },
        ..Default::default()
    };

    let result = GenericTestBuilder::new(
        config,
        |_| TcpExecutor::with_pool(Duration::from_secs(5), 200, ConnectionPolicy::AlwaysNew),
        Box::new(move |_| {
            Box::new(
                SimpleTcpGenerator::new(targets.clone(), payload.clone(), TcpFraming::Raw, true)
                    .with_mode(TcpTestMode::CRR)
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

    assert_eq!(
        result.total_errors, 0,
        "expected 0 errors in CRR mode, got {}",
        result.total_errors
    );
    assert!(
        result.total_requests > 0,
        "expected some CRR requests completed"
    );

    // CRR = one connection per request. Server connections_accepted should
    // be close to client total_requests.
    std::thread::sleep(Duration::from_millis(500));
    let metrics = server.metrics_snapshot();
    let ratio = metrics.connections_accepted as f64 / result.total_requests as f64;
    assert!(
        (0.9..=1.1).contains(&ratio),
        "CRR: connections_accepted ({}) should be within 10% of total_requests ({}), ratio={:.2}",
        metrics.connections_accepted,
        result.total_requests,
        ratio
    );
}

#[test]
fn test_v2_backward_compat() {
    // v2 header with standard RR mode and no injection flags should behave
    // identically to the old v1 header.
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
        duration: Duration::from_secs(5),
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

    assert_eq!(
        result.total_errors, 0,
        "expected 0 errors with v2 RR mode, got {}",
        result.total_errors
    );
    assert!(
        result.total_requests > 0,
        "expected some requests completed with v2 header"
    );
}

#[test]
fn test_latency_injection() {
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
                    .with_response_size(256)
                    .with_latency_us(5000), // 5ms injection
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
        "expected 0 errors with latency injection, got {}",
        result.total_errors
    );
    assert!(
        result.total_requests > 0,
        "expected some requests completed"
    );
    // p50 should reflect the 5ms injection floor.
    assert!(
        result.latency_p50 >= Duration::from_millis(5),
        "p50 latency should be >= 5ms with 5ms injection, got {:?}",
        result.latency_p50
    );
    assert!(
        result.latency_p50 < Duration::from_millis(25),
        "p50 latency should be < 25ms (bounded above injection), got {:?}",
        result.latency_p50
    );
}

#[test]
fn test_error_injection() {
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
        duration: Duration::from_secs(10),
        rate: RateConfig::Static { rps: 2000.0 },
        num_cores: 1,
        error_status_threshold: 0,
        connections: netanvil_types::ConnectionConfig {
            connection_policy: ConnectionPolicy::AlwaysNew,
            request_timeout: Duration::from_secs(5),
            ..Default::default()
        },
        ..Default::default()
    };

    let result = GenericTestBuilder::new(
        config,
        |_| TcpExecutor::with_pool(Duration::from_secs(5), 200, ConnectionPolicy::AlwaysNew),
        Box::new(move |_| {
            Box::new(
                SimpleTcpGenerator::new(targets.clone(), payload.clone(), TcpFraming::Raw, true)
                    .with_mode(TcpTestMode::RR)
                    .with_request_size(64)
                    .with_response_size(256)
                    .with_error_rate(1000), // 10% error rate
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
        "expected some requests completed"
    );

    // With 10% error injection, observed error rate should be between 5% and 20%.
    let error_rate = result.total_errors as f64 / result.total_requests as f64;
    assert!(
        (0.05..=0.20).contains(&error_rate),
        "error rate should be between 5% and 20% with 10% injection, got {:.1}% ({} errors / {} requests)",
        error_rate * 100.0,
        result.total_errors,
        result.total_requests
    );
}

// ---------------------------------------------------------------------------
// Phase 6C: UDP Protocol Modes
// ---------------------------------------------------------------------------

/// Build a v2 protocol header for UDP datagrams.
fn build_udp_protocol_header(
    mode: u8,
    request_size: u16,
    response_size: u32,
    latency_us: Option<u32>,
    error_rate: Option<u32>,
) -> Vec<u8> {
    use netanvil_test_servers::protocol;

    let mut flags: u8 = 0;
    let mut header_len = protocol::HEADER_MIN;
    if latency_us.is_some() {
        flags |= protocol::FLAG_LATENCY;
        header_len += 4;
    }
    if error_rate.is_some() {
        flags |= protocol::FLAG_ERROR;
        header_len += 4;
    }

    let mut buf = Vec::with_capacity(header_len);
    buf.push(protocol::VERSION_V2);
    buf.push(header_len as u8);
    buf.push(mode);
    buf.push(flags);
    buf.extend_from_slice(&request_size.to_be_bytes());
    buf.extend_from_slice(&response_size.to_be_bytes());
    if let Some(lat) = latency_us {
        buf.extend_from_slice(&lat.to_be_bytes());
    }
    if let Some(err) = error_rate {
        buf.extend_from_slice(&err.to_be_bytes());
    }
    buf
}

#[compio::test]
#[serial_test::serial(udp)]
async fn test_udp_rr_unconnected() {
    use compio::buf::BufResult;
    use netanvil_test_servers::protocol;

    let server = netanvil_test_servers::udp::start_udp_echo();
    let socket = compio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();

    // Send datagram with RR header: response_size=128
    let mut payload = build_udp_protocol_header(protocol::MODE_RR, 64, 128, None, None);
    payload.extend_from_slice(&[0xAB; 64]); // request body

    let BufResult(result, _) = socket.send_to(payload, server.addr).await;
    result.unwrap();

    // Should receive a 128-byte response.
    let recv_buf = vec![0u8; 256];
    let recv_result =
        compio::time::timeout(Duration::from_secs(2), socket.recv_from(recv_buf)).await;
    let BufResult(Ok((n, _)), _) = recv_result.unwrap() else {
        panic!("recv failed");
    };
    assert_eq!(n, 128, "RR response should be 128 bytes, got {}", n);
}

#[compio::test]
#[serial_test::serial(udp)]
async fn test_udp_rr_connected() {
    use compio::buf::BufResult;
    use netanvil_test_servers::protocol;

    let server = netanvil_test_servers::udp::start_udp_echo_with_config(
        netanvil_test_servers::ServerConfig {
            addr: "127.0.0.1:0".to_string(),
            udp_idle_timeout_secs: 30,
            ..netanvil_test_servers::ServerConfig::udp_default()
        },
    );
    let socket = compio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();

    // First datagram: RR header with response_size=256
    let mut payload = build_udp_protocol_header(protocol::MODE_RR, 64, 256, None, None);
    payload.extend_from_slice(&[0xAB; 64]);

    let BufResult(result, _) = socket.send_to(payload, server.addr).await;
    result.unwrap();

    // First response from listener: 256 bytes.
    let recv_buf = vec![0u8; 512];
    let recv_result =
        compio::time::timeout(Duration::from_secs(2), socket.recv_from(recv_buf)).await;
    let BufResult(Ok((n, _)), _) = recv_result.unwrap() else {
        panic!("recv failed");
    };
    assert_eq!(n, 256, "first RR response should be 256 bytes, got {}", n);

    // Wait for connected handler to be ready.
    compio::time::sleep(Duration::from_millis(50)).await;

    // Subsequent datagrams (no header) should still get response_size responses
    // since the session mode is stored.
    let payload2 = vec![0xCD; 64];
    let BufResult(result, _) = socket.send_to(payload2, server.addr).await;
    result.unwrap();

    let recv_buf = vec![0u8; 512];
    let recv_result =
        compio::time::timeout(Duration::from_secs(2), socket.recv_from(recv_buf)).await;
    let BufResult(Ok((n, _)), _) = recv_result.unwrap() else {
        panic!("second recv failed");
    };
    assert_eq!(
        n, 256,
        "subsequent RR response should be 256 bytes, got {}",
        n
    );
}

#[compio::test]
#[serial_test::serial(udp)]
async fn test_udp_sink_unconnected() {
    use compio::buf::BufResult;
    use netanvil_test_servers::protocol;

    let server = netanvil_test_servers::udp::start_udp_echo();
    let socket = compio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();

    // Send SINK header — should get no response.
    let payload = build_udp_protocol_header(protocol::MODE_SINK, 0, 0, None, None);
    let BufResult(result, _) = socket.send_to(payload, server.addr).await;
    result.unwrap();

    // Verify no response within 200ms.
    let recv_buf = vec![0u8; 256];
    let recv_result =
        compio::time::timeout(Duration::from_millis(200), socket.recv_from(recv_buf)).await;
    assert!(
        recv_result.is_err(),
        "SINK mode should not send any response"
    );
}

#[compio::test]
#[serial_test::serial(udp)]
async fn test_udp_sink_connected() {
    use compio::buf::BufResult;
    use netanvil_test_servers::protocol;

    let server = netanvil_test_servers::udp::start_udp_echo_with_config(
        netanvil_test_servers::ServerConfig {
            addr: "127.0.0.1:0".to_string(),
            udp_idle_timeout_secs: 30,
            ..netanvil_test_servers::ServerConfig::udp_default()
        },
    );
    let socket = compio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();

    // Send SINK header.
    let payload = build_udp_protocol_header(protocol::MODE_SINK, 0, 0, None, None);
    let BufResult(result, _) = socket.send_to(payload, server.addr).await;
    result.unwrap();

    // No response expected.
    let recv_buf = vec![0u8; 256];
    let recv_result =
        compio::time::timeout(Duration::from_millis(200), socket.recv_from(recv_buf)).await;
    assert!(
        recv_result.is_err(),
        "SINK mode (connected) should not send any response"
    );

    // Send more datagrams — still no response (session is SINK).
    compio::time::sleep(Duration::from_millis(50)).await;
    let payload2 = vec![0xAB; 64];
    let BufResult(result, _) = socket.send_to(payload2, server.addr).await;
    result.unwrap();

    let recv_buf = vec![0u8; 256];
    let recv_result =
        compio::time::timeout(Duration::from_millis(200), socket.recv_from(recv_buf)).await;
    assert!(
        recv_result.is_err(),
        "SINK mode (connected) should still not respond after session established"
    );
}

#[compio::test]
#[serial_test::serial(udp)]
async fn test_udp_source_connected() {
    use compio::buf::BufResult;
    use netanvil_test_servers::protocol;

    let server = netanvil_test_servers::udp::start_udp_echo_with_config(
        netanvil_test_servers::ServerConfig {
            addr: "127.0.0.1:0".to_string(),
            udp_idle_timeout_secs: 30,
            ..netanvil_test_servers::ServerConfig::udp_default()
        },
    );
    let socket = compio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();

    // Send SOURCE header: response_size=256
    let payload = build_udp_protocol_header(protocol::MODE_SOURCE, 0, 256, None, None);
    let BufResult(result, _) = socket.send_to(payload, server.addr).await;
    result.unwrap();

    // Should receive multiple 256-byte datagrams without sending more.
    let mut received = 0;
    for _ in 0..5 {
        let recv_buf = vec![0u8; 512];
        let recv_result =
            compio::time::timeout(Duration::from_secs(2), socket.recv_from(recv_buf)).await;
        match recv_result {
            Ok(BufResult(Ok((n, _)), _)) => {
                assert_eq!(n, 256, "SOURCE datagram should be 256 bytes, got {}", n);
                received += 1;
            }
            _ => break,
        }
    }
    assert!(
        received >= 3,
        "SOURCE mode should continuously send datagrams, only received {}",
        received
    );
}

#[compio::test]
#[serial_test::serial(udp)]
async fn test_udp_source_unconnected_rejected() {
    use compio::buf::BufResult;
    use netanvil_test_servers::protocol;

    // Unconnected mode (idle_timeout=0).
    let server = netanvil_test_servers::udp::start_udp_echo();
    let socket = compio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();

    // Send SOURCE header in unconnected mode — should be rejected.
    let payload = build_udp_protocol_header(protocol::MODE_SOURCE, 0, 256, None, None);
    let BufResult(result, _) = socket.send_to(payload, server.addr).await;
    result.unwrap();

    // No response expected.
    let recv_buf = vec![0u8; 512];
    let recv_result =
        compio::time::timeout(Duration::from_millis(200), socket.recv_from(recv_buf)).await;
    assert!(
        recv_result.is_err(),
        "SOURCE mode should be rejected in unconnected mode"
    );
}

#[compio::test]
#[serial_test::serial(udp)]
async fn test_udp_bidir_connected() {
    use compio::buf::BufResult;
    use netanvil_test_servers::protocol;

    let server = netanvil_test_servers::udp::start_udp_echo_with_config(
        netanvil_test_servers::ServerConfig {
            addr: "127.0.0.1:0".to_string(),
            udp_idle_timeout_secs: 30,
            ..netanvil_test_servers::ServerConfig::udp_default()
        },
    );
    let socket = compio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();

    // Send BIDIR header: response_size=128
    let payload = build_udp_protocol_header(protocol::MODE_BIDIR, 0, 128, None, None);
    let BufResult(result, _) = socket.send_to(payload, server.addr).await;
    result.unwrap();

    // Should receive continuous 128-byte datagrams (SOURCE side of BIDIR).
    let mut received = 0;
    for _ in 0..5 {
        let recv_buf = vec![0u8; 256];
        let recv_result =
            compio::time::timeout(Duration::from_secs(2), socket.recv_from(recv_buf)).await;
        match recv_result {
            Ok(BufResult(Ok((n, _)), _)) => {
                assert_eq!(n, 128, "BIDIR datagram should be 128 bytes, got {}", n);
                received += 1;
            }
            _ => break,
        }
    }
    assert!(
        received >= 3,
        "BIDIR mode should continuously send datagrams, only received {}",
        received
    );

    // Also send datagrams while receiving (SINK side should absorb them).
    let payload2 = vec![0xAB; 64];
    let BufResult(result, _) = socket.send_to(payload2, server.addr).await;
    result.unwrap();

    // Can still receive SOURCE datagrams after sending.
    let recv_buf = vec![0u8; 256];
    let recv_result =
        compio::time::timeout(Duration::from_secs(2), socket.recv_from(recv_buf)).await;
    assert!(
        recv_result.is_ok(),
        "BIDIR should still send after receiving client datagrams"
    );
}

#[compio::test]
#[serial_test::serial(udp)]
async fn test_udp_bidir_unconnected_rejected() {
    use compio::buf::BufResult;
    use netanvil_test_servers::protocol;

    let server = netanvil_test_servers::udp::start_udp_echo();
    let socket = compio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();

    // Send BIDIR header in unconnected mode — should be rejected.
    let payload = build_udp_protocol_header(protocol::MODE_BIDIR, 0, 128, None, None);
    let BufResult(result, _) = socket.send_to(payload, server.addr).await;
    result.unwrap();

    let recv_buf = vec![0u8; 256];
    let recv_result =
        compio::time::timeout(Duration::from_millis(200), socket.recv_from(recv_buf)).await;
    assert!(
        recv_result.is_err(),
        "BIDIR mode should be rejected in unconnected mode"
    );
}

#[compio::test]
#[serial_test::serial(udp)]
async fn test_udp_echo_fallback() {
    use compio::buf::BufResult;

    let server = netanvil_test_servers::udp::start_udp_echo();
    let socket = compio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();

    // Send plain datagram (no protocol sentinel) — should echo back.
    let payload = vec![0xAB; 100];
    let BufResult(result, _) = socket.send_to(payload, server.addr).await;
    result.unwrap();

    let recv_buf = vec![0u8; 256];
    let recv_result =
        compio::time::timeout(Duration::from_secs(2), socket.recv_from(recv_buf)).await;
    let BufResult(Ok((n, _)), buf) = recv_result.unwrap() else {
        panic!("echo recv failed");
    };
    assert_eq!(n, 100, "echo should return same size, got {}", n);
    assert!(
        buf[..n].iter().all(|&b| b == 0xAB),
        "echo should return same data"
    );
}

#[compio::test]
#[serial_test::serial(udp)]
async fn test_udp_rr_error_injection() {
    use compio::buf::BufResult;
    use netanvil_test_servers::protocol;

    let server = netanvil_test_servers::udp::start_udp_echo();
    let socket = compio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();

    // RR with 100% error rate: response_size=128, error_rate=10000
    let mut payload = build_udp_protocol_header(protocol::MODE_RR, 64, 128, None, Some(10000));
    payload.extend_from_slice(&[0xAB; 64]);

    let BufResult(result, _) = socket.send_to(payload, server.addr).await;
    result.unwrap();

    let recv_buf = vec![0u8; 256];
    let recv_result =
        compio::time::timeout(Duration::from_secs(2), socket.recv_from(recv_buf)).await;
    let BufResult(Ok((n, _)), buf) = recv_result.unwrap() else {
        panic!("error response recv failed");
    };

    // Error response should be half size (64 bytes) filled with 0xEE.
    assert_eq!(
        n, 64,
        "error response should be half of 128 = 64 bytes, got {}",
        n
    );
    assert!(
        buf[..n].iter().all(|&b| b == 0xEE),
        "error response should be filled with 0xEE"
    );
}

#[compio::test]
#[serial_test::serial(udp)]
async fn test_udp_crr_treated_as_rr() {
    use compio::buf::BufResult;
    use netanvil_test_servers::protocol;

    let server = netanvil_test_servers::udp::start_udp_echo();
    let socket = compio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();

    // Send CRR header — should be treated as RR.
    let mut payload = build_udp_protocol_header(protocol::MODE_CRR, 64, 128, None, None);
    payload.extend_from_slice(&[0xAB; 64]);

    let BufResult(result, _) = socket.send_to(payload, server.addr).await;
    result.unwrap();

    let recv_buf = vec![0u8; 256];
    let recv_result =
        compio::time::timeout(Duration::from_secs(2), socket.recv_from(recv_buf)).await;
    let BufResult(Ok((n, _)), _) = recv_result.unwrap() else {
        panic!("CRR-as-RR recv failed");
    };
    assert_eq!(
        n, 128,
        "CRR should be treated as RR, expected 128 bytes, got {}",
        n
    );
}

// ---------------------------------------------------------------------------
// Phase 6B: UDP Behavior Control
// ---------------------------------------------------------------------------

#[test]
#[serial_test::serial(udp)]
fn test_udp_drop_simulation() {
    use netanvil_udp::{SimpleUdpGenerator, UdpExecutor, UdpNoopTransformer, UdpRequestSpec};

    // Validate server-side drop simulation via server metrics. The client-side
    // error rate is unreliable because the UDP executor's response mixing
    // amplifies drops: when the server drops task A's echo, task A may receive
    // task B's echo (seq mismatch), causing task B to time out too.
    let server = netanvil_test_servers::TestServer::builder()
        .udp(0)
        .workers(1)
        .pin_cores(false)
        .udp_drop_rate(1000) // 10%
        .build();
    let udp_addr = server.udp_addr().unwrap();
    let targets = vec![udp_addr];
    let payload = vec![0u8; 64];

    let config = TestConfig {
        targets: vec![format!("{}", udp_addr)],
        duration: Duration::from_secs(5),
        rate: RateConfig::Static { rps: 1000.0 },
        num_cores: 1,
        error_status_threshold: 0,
        ..Default::default()
    };

    let _result = GenericTestBuilder::new(
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

    // Server should have received datagrams
    assert!(
        metrics.datagrams_received > 0,
        "expected server datagrams_received > 0, got {}",
        metrics.datagrams_received
    );

    // Server should report dropped datagrams
    assert!(
        metrics.datagrams_dropped > 0,
        "expected server datagrams_dropped > 0, got {}",
        metrics.datagrams_dropped
    );

    // Server-side drop ratio should be approximately 10%
    let server_drop_rate = metrics.datagrams_dropped as f64 / metrics.datagrams_received as f64;
    assert!(
        (0.05..=0.20).contains(&server_drop_rate),
        "server drop rate should be ~10%, got {:.1}% ({} dropped / {} received)",
        server_drop_rate * 100.0,
        metrics.datagrams_dropped,
        metrics.datagrams_received
    );
}

#[test]
#[serial_test::serial(udp)]
fn test_udp_latency_injection_udp() {
    use netanvil_udp::{SimpleUdpGenerator, UdpExecutor, UdpNoopTransformer, UdpRequestSpec};

    // In unconnected mode the echo loop is serial: recv → sleep(2ms) → send.
    // Max throughput ≈ 475 pkt/s. Keep rate well below to avoid backlog.
    let server = netanvil_test_servers::TestServer::builder()
        .udp(0)
        .workers(1)
        .pin_cores(false)
        .udp_latency_us(2000) // 2ms
        .build();
    let udp_addr = server.udp_addr().unwrap();
    let targets = vec![udp_addr];
    let payload = vec![0u8; 64];

    let config = TestConfig {
        targets: vec![format!("{}", udp_addr)],
        duration: Duration::from_secs(8),
        rate: RateConfig::Static { rps: 200.0 },
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
        "expected 0 errors with UDP latency injection, got {}",
        result.total_errors
    );
    assert!(
        result.latency_p50 >= Duration::from_millis(2),
        "p50 latency should be >= 2ms with 2ms injection, got {:?}",
        result.latency_p50
    );
    assert!(
        result.latency_p50 < Duration::from_millis(15),
        "p50 latency should be < 15ms (bounded above), got {:?}",
        result.latency_p50
    );
}

#[test]
#[serial_test::serial(udp)]
fn test_udp_pacing() {
    use netanvil_udp::{SimpleUdpGenerator, UdpExecutor, UdpNoopTransformer, UdpRequestSpec};

    // 100 KB/s pacing with 64-byte payloads. Client sends at 10k RPS
    // (unconstrained ~640 KB/s), so pacing should clearly constrain.
    let server = netanvil_test_servers::TestServer::builder()
        .udp(0)
        .workers(1)
        .pin_cores(false)
        .udp_idle_timeout(30)
        .udp_pacing_bps(102400) // 100 KB/s
        .build();
    let udp_addr = server.udp_addr().unwrap();
    let targets = vec![udp_addr];
    let payload = vec![0u8; 64];

    let config = TestConfig {
        targets: vec![format!("{}", udp_addr)],
        duration: Duration::from_secs(10),
        rate: RateConfig::Static { rps: 10000.0 },
        num_cores: 1,
        error_status_threshold: 0,
        // Cap concurrency so the kernel's io_uring doesn't overflow pending
        // UDP recvs into io-wq workers (they'd park in blocking recvmsg and
        // stall ring teardown at test end).
        connections: netanvil_types::ConnectionConfig {
            max_in_flight_per_core: 1024,
            ..Default::default()
        },
        ..Default::default()
    };

    let _result = GenericTestBuilder::new(
        config,
        |_| UdpExecutor::with_timeout(Duration::from_millis(200)),
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

    // Server bytes_sent per second should be near the pacing rate.
    // Allow wide tolerance for initial burst and timer jitter.
    let bytes_per_sec = metrics.bytes_sent as f64 / 10.0;
    assert!(
        bytes_per_sec >= 60_000.0,
        "bytes/sec too low: {:.0} (expected >= 60000 with 100 KB/s pacing)",
        bytes_per_sec
    );
    assert!(
        bytes_per_sec <= 180_000.0,
        "bytes/sec too high: {:.0} (expected <= 180000 with 100 KB/s pacing)",
        bytes_per_sec
    );
}

#[test]
#[serial_test::serial(udp)]
fn test_udp_behavior_backward_compat() {
    use netanvil_udp::{SimpleUdpGenerator, UdpExecutor, UdpNoopTransformer, UdpRequestSpec};

    // All behavior fields default to 0 — no drop, no latency, no pacing.
    let server = netanvil_test_servers::TestServer::builder()
        .udp(0)
        .workers(1)
        .pin_cores(false)
        .build();
    let udp_addr = server.udp_addr().unwrap();
    let targets = vec![udp_addr];
    let payload = vec![0u8; 64];

    let config = TestConfig {
        targets: vec![format!("{}", udp_addr)],
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
        "expected 0 errors with default UDP behavior config, got {}",
        result.total_errors
    );
    assert!(
        result.total_requests > 0,
        "expected some requests completed"
    );
}
