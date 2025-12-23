//! Distributed behavioral tests for TCP/UDP protocol integration.
//!
//! Spins up in-process agents + a TCP/UDP test server, then runs a
//! DistributedCoordinator to verify end-to-end distributed protocol testing.

use std::time::Duration;

use netanvil_api::AgentServer;
use netanvil_distributed::{
    DistributedCoordinator, HttpMetricsFetcher, HttpNodeCommander, StaticDiscovery,
};
use netanvil_types::{ConnectionConfig, ProtocolConfig, RateConfig, TestConfig};

fn start_agent(port: u16) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name(format!("agent-{port}"))
        .spawn(move || {
            let server = AgentServer::new(&format!("127.0.0.1:{port}"), 1, None).expect("start agent");
            server.run();
        })
        .unwrap()
}

fn make_tcp_test_config(
    tcp_addr: std::net::SocketAddr,
    rps: f64,
    duration_secs: u64,
) -> TestConfig {
    TestConfig {
        targets: vec![format!("tcp://{}", tcp_addr)],
        duration: Duration::from_secs(duration_secs),
        rate: RateConfig::Static { rps },
        num_cores: 1,
        error_status_threshold: 0,
        connections: ConnectionConfig {
            request_timeout: Duration::from_secs(10),
            ..Default::default()
        },
        control_interval: Duration::from_secs(1),
        protocol: Some(ProtocolConfig::Tcp {
            mode: "rr".to_string(),
            payload_hex: "50494e47".to_string(), // "PING"
            framing: "raw".to_string(),
            request_size: netanvil_types::ValueDistribution::Fixed(4),
            response_size: netanvil_types::ValueDistribution::Fixed(4),
        }),
        ..Default::default()
    }
}

#[test]
fn distributed_tcp_test_with_two_agents() {
    let tcp_server = netanvil_test_servers::tcp::start_tcp_echo();

    let port1 = 19190;
    let port2 = 19191;
    let _agent1 = start_agent(port1);
    let _agent2 = start_agent(port2);

    // Give agents time to start
    std::thread::sleep(Duration::from_millis(500));

    let config = make_tcp_test_config(tcp_server.addr, 100.0, 8);

    let discovery = StaticDiscovery::new(vec![
        format!("127.0.0.1:{port1}"),
        format!("127.0.0.1:{port2}"),
    ]);
    let fetcher = HttpMetricsFetcher::new(Duration::from_secs(5));
    let commander = HttpNodeCommander::new(Duration::from_secs(10));
    let rate_controller = Box::new(netanvil_core::StaticRateController::new(100.0));

    let mut coordinator =
        DistributedCoordinator::new(discovery, fetcher, commander, config, rate_controller);

    let result = coordinator.run();

    let total = result.total_requests;
    let duration_secs = result.duration.as_secs_f64();

    eprintln!("Distributed TCP test: {total} requests in {duration_secs:.1}s");
    for (id, m) in &result.nodes {
        eprintln!(
            "  {id}: {} requests, {:.1} RPS",
            m.total_requests, m.current_rps
        );
    }

    // Should have generated requests (100 RPS for 3s = ~300 expected)
    assert!(
        total > 50,
        "expected >50 requests across 2 agents, got {total}"
    );

    // Both agents should have contributed
    assert!(
        result.nodes.len() == 2,
        "expected metrics from 2 nodes, got {}",
        result.nodes.len()
    );

    for (id, m) in &result.nodes {
        assert!(
            m.total_requests > 0,
            "node {id} should have contributed TCP requests, got {}",
            m.total_requests
        );
    }
}

#[test]
fn distributed_udp_test_with_two_agents() {
    let udp_server = netanvil_test_servers::udp::start_udp_echo();

    let port1 = 19192;
    let port2 = 19193;
    let _agent1 = start_agent(port1);
    let _agent2 = start_agent(port2);

    std::thread::sleep(Duration::from_millis(500));

    let config = TestConfig {
        targets: vec![format!("udp://{}", udp_server.addr)],
        duration: Duration::from_secs(8),
        rate: RateConfig::Static { rps: 80.0 },
        num_cores: 1,
        error_status_threshold: 0,
        connections: ConnectionConfig {
            request_timeout: Duration::from_secs(10),
            ..Default::default()
        },
        control_interval: Duration::from_secs(1),
        protocol: Some(ProtocolConfig::Udp {
            payload_hex: "48454c4c4f".to_string(), // "HELLO"
            expect_response: true,
        }),
        ..Default::default()
    };

    let discovery = StaticDiscovery::new(vec![
        format!("127.0.0.1:{port1}"),
        format!("127.0.0.1:{port2}"),
    ]);
    let fetcher = HttpMetricsFetcher::new(Duration::from_secs(5));
    let commander = HttpNodeCommander::new(Duration::from_secs(10));
    let rate_controller = Box::new(netanvil_core::StaticRateController::new(80.0));

    let mut coordinator =
        DistributedCoordinator::new(discovery, fetcher, commander, config, rate_controller);

    let result = coordinator.run();

    let total = result.total_requests;
    eprintln!(
        "Distributed UDP test: {total} requests in {:.1}s",
        result.duration.as_secs_f64()
    );

    assert!(
        total > 20,
        "expected >20 UDP requests across 2 agents, got {total}"
    );

    assert!(
        result.nodes.len() == 2,
        "expected metrics from 2 nodes, got {}",
        result.nodes.len()
    );

    for (id, m) in &result.nodes {
        assert!(
            m.total_requests > 0,
            "node {id} should have contributed UDP requests, got {}",
            m.total_requests
        );
    }
}

#[test]
fn distributed_tcp_test_completes_within_duration() {
    let tcp_server = netanvil_test_servers::tcp::start_tcp_echo();

    let port = 19194;
    let _agent = start_agent(port);
    std::thread::sleep(Duration::from_millis(500));

    let config = make_tcp_test_config(tcp_server.addr, 50.0, 3);

    let discovery = StaticDiscovery::new(vec![format!("127.0.0.1:{port}")]);
    let fetcher = HttpMetricsFetcher::new(Duration::from_secs(5));
    let commander = HttpNodeCommander::new(Duration::from_secs(10));
    let rate_controller = Box::new(netanvil_core::StaticRateController::new(50.0));

    let mut coordinator =
        DistributedCoordinator::new(discovery, fetcher, commander, config, rate_controller);

    let before = std::time::Instant::now();
    let result = coordinator.run();
    let elapsed = before.elapsed();

    // Should complete in roughly 3 seconds (not much longer)
    assert!(
        elapsed < Duration::from_secs(10),
        "distributed TCP test should complete within duration, took {:?}",
        elapsed
    );

    assert!(
        result.total_requests > 0,
        "should have generated TCP requests, got {}",
        result.total_requests
    );
}
