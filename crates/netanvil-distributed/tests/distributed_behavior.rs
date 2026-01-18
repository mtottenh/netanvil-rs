//! Distributed behavioral tests.
//!
//! Spins up in-process agents + a target server, then runs a
//! DistributedCoordinator to verify end-to-end distributed load testing.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use netanvil_api::AgentServer;
use netanvil_distributed::{
    DistributedCoordinator, HttpMetricsFetcher, HttpNodeCommander, StaticDiscovery,
};
use netanvil_types::{ConnectionConfig, ProtocolConfig, RateConfig, TestConfig};

fn start_target_server() -> (SocketAddr, Arc<AtomicU64>) {
    let (addr_tx, addr_rx) = std::sync::mpsc::channel();
    let count = Arc::new(AtomicU64::new(0));
    let server_count = count.clone();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            use axum::extract::State;
            use axum::routing::get;

            let app = axum::Router::new()
                .route(
                    "/",
                    get(|State(c): State<Arc<AtomicU64>>| async move {
                        c.fetch_add(1, Ordering::Relaxed);
                        "OK"
                    }),
                )
                .with_state(server_count);

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            addr_tx.send(addr).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });

    let addr = addr_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    (addr, count)
}

fn start_agent(port: u16) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name(format!("agent-{port}"))
        .spawn(move || {
            let server =
                AgentServer::new(&format!("127.0.0.1:{port}"), 1, None).expect("start agent");
            server.run();
        })
        .unwrap()
}

fn make_test_config(target_addr: SocketAddr, rps: f64, duration_secs: u64) -> TestConfig {
    TestConfig {
        targets: vec![format!("http://{}/", target_addr)],
        duration: Duration::from_secs(duration_secs),
        rate: RateConfig::Static { rps },
        num_cores: 1,
        connections: ConnectionConfig {
            request_timeout: Duration::from_secs(10),
            ..Default::default()
        },
        control_interval: Duration::from_secs(1),
        ..Default::default()
    }
}

#[test]
fn distributed_test_with_two_agents() {
    // Start target server
    let (target_addr, server_count) = start_target_server();

    // Start two agents on ephemeral ports
    // Use high ports to avoid conflicts
    let port1 = 19090;
    let port2 = 19091;
    let _agent1 = start_agent(port1);
    let _agent2 = start_agent(port2);

    // Give agents time to start
    std::thread::sleep(Duration::from_millis(500));

    // Build config — duration must be long enough for agents to start generating
    // requests and for the distributed coordinator to fetch metrics (control_interval=1s).
    let config = make_test_config(target_addr, 200.0, 8);

    // Build distributed coordinator
    let discovery = StaticDiscovery::new(vec![
        format!("127.0.0.1:{port1}"),
        format!("127.0.0.1:{port2}"),
    ]);
    let fetcher = HttpMetricsFetcher::new(Duration::from_secs(5));
    let commander = HttpNodeCommander::new(Duration::from_secs(10));

    let rate_controller = Box::new(netanvil_core::StaticRateController::new(200.0));

    let mut coordinator =
        DistributedCoordinator::new(discovery, fetcher, commander, config, rate_controller);

    // Run the test
    let result = coordinator.run();

    // Verify results
    let total = result.total_requests;
    let duration_secs = result.duration.as_secs_f64();

    eprintln!("Distributed test: {total} requests in {duration_secs:.1}s");
    for (id, m) in &result.nodes {
        eprintln!(
            "  {id}: {} requests, {:.1} RPS",
            m.total_requests, m.current_rps
        );
    }

    // Should have generated a reasonable number of requests
    // 200 RPS for 8 seconds = ~1600 expected
    assert!(
        total > 300,
        "expected >300 requests across 2 agents, got {total}"
    );

    // Both agents should have contributed
    assert!(
        result.nodes.len() == 2,
        "expected metrics from 2 nodes, got {}",
        result.nodes.len()
    );

    for (id, m) in &result.nodes {
        assert!(
            m.total_requests > 50,
            "node {id} should have contributed requests, got {}",
            m.total_requests
        );
    }

    // Server should have seen the requests
    let server_total = server_count.load(Ordering::Relaxed);
    assert!(
        server_total > 300,
        "server should have received >300 requests, got {server_total}"
    );
}

#[test]
fn distributed_stop_terminates_all_agents() {
    let (target_addr, _server_count) = start_target_server();

    let port = 19092;
    let _agent = start_agent(port);
    std::thread::sleep(Duration::from_millis(500));

    // Short test — verify the coordinator stops within the duration
    let config = make_test_config(target_addr, 100.0, 8);

    let discovery = StaticDiscovery::new(vec![format!("127.0.0.1:{port}")]);
    let fetcher = HttpMetricsFetcher::new(Duration::from_secs(5));
    let commander = HttpNodeCommander::new(Duration::from_secs(10));
    let rate_controller = Box::new(netanvil_core::StaticRateController::new(100.0));

    let mut coordinator =
        DistributedCoordinator::new(discovery, fetcher, commander, config, rate_controller);

    let before = std::time::Instant::now();
    let result = coordinator.run();
    let elapsed = before.elapsed();

    // Should complete in roughly 3 seconds (not 60)
    assert!(
        elapsed < Duration::from_secs(10),
        "distributed test should complete within duration, took {:?}",
        elapsed
    );

    assert!(
        result.total_requests > 100,
        "should have generated requests, got {}",
        result.total_requests
    );
}

#[test]
fn distributed_dns_test_with_two_agents() {
    // Start DNS echo server
    let dns_server = netanvil_test_servers::dns::start_dns_echo();

    // Start two agents
    let port1 = 19093;
    let port2 = 19094;
    let _agent1 = start_agent(port1);
    let _agent2 = start_agent(port2);
    std::thread::sleep(Duration::from_millis(500));

    // Build DNS test config
    let config = TestConfig {
        targets: vec![format!("dns://{}", dns_server.addr)],
        duration: Duration::from_secs(8),
        rate: RateConfig::Static { rps: 200.0 },
        num_cores: 1,
        connections: ConnectionConfig {
            request_timeout: Duration::from_secs(5),
            ..Default::default()
        },
        control_interval: Duration::from_secs(1),
        error_status_threshold: 0,
        protocol: Some(ProtocolConfig::Dns {
            domains: "example.com,test.com,foo.bar".to_string(),
            query_type: "A".to_string(),
            recursion: true,
        }),
        ..Default::default()
    };

    let discovery = StaticDiscovery::new(vec![
        format!("127.0.0.1:{port1}"),
        format!("127.0.0.1:{port2}"),
    ]);
    let fetcher = HttpMetricsFetcher::new(Duration::from_secs(5));
    let commander = HttpNodeCommander::new(Duration::from_secs(10));
    let rate_controller = Box::new(netanvil_core::StaticRateController::new(200.0));

    let mut coordinator =
        DistributedCoordinator::new(discovery, fetcher, commander, config, rate_controller);

    let result = coordinator.run();

    eprintln!(
        "Distributed DNS test: {} requests in {:.1}s",
        result.total_requests,
        result.duration.as_secs_f64()
    );
    for (id, m) in &result.nodes {
        eprintln!(
            "  {id}: {} requests, {:.1} RPS",
            m.total_requests, m.current_rps
        );
    }

    // Should have generated a reasonable number of DNS queries
    assert!(
        result.total_requests > 200,
        "expected >200 DNS requests across 2 agents, got {}",
        result.total_requests
    );

    // Both agents should have contributed
    assert_eq!(
        result.nodes.len(),
        2,
        "expected metrics from 2 nodes, got {}",
        result.nodes.len()
    );

    for (id, m) in &result.nodes {
        assert!(
            m.total_requests > 30,
            "node {id} should have contributed DNS requests, got {}",
            m.total_requests
        );
    }
}
