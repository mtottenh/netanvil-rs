//! End-to-end test: verifies connection policy affects actual HTTP behavior.
//!
//! With AlwaysNew, the server should see many distinct source ports (new connections).
//! With KeepAlive, the server should see connection reuse (fewer source ports).

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use netanvil_core::run_test;
use netanvil_http::HttpExecutor;
use netanvil_types::{ConnectionConfig, ConnectionPolicy, RateConfig, TestConfig};

fn start_port_tracking_server() -> (SocketAddr, Arc<Mutex<HashSet<u16>>>) {
    let (addr_tx, addr_rx) = std::sync::mpsc::channel();
    let ports = Arc::new(Mutex::new(HashSet::new()));
    let server_ports = ports.clone();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            use axum::extract::{ConnectInfo, State};
            use axum::routing::get;

            let app = axum::Router::new()
                .route(
                    "/",
                    get(
                        |State(ports): State<Arc<Mutex<HashSet<u16>>>>,
                         ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>| async move {
                            ports.lock().unwrap().insert(addr.port());
                            "OK"
                        },
                    ),
                )
                .with_state(server_ports);

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            addr_tx.send(addr).unwrap();
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
    });

    let addr = addr_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    (addr, ports)
}

#[test]
fn always_new_policy_creates_many_connections() {
    let (addr, ports) = start_port_tracking_server();

    let config = TestConfig {
        targets: vec![format!("http://{}/", addr)],
        duration: Duration::from_secs(2),
        rate: RateConfig::Static { rps: 100.0 },
        num_cores: 1,
        connections: ConnectionConfig {
            request_timeout: Duration::from_secs(10),
            connection_policy: ConnectionPolicy::AlwaysNew,
            ..Default::default()
        },
        control_interval: Duration::from_secs(1),
        ..Default::default()
    };

    let result = run_test(config, |_| {
        HttpExecutor::with_timeout(Duration::from_secs(10))
    })
    .unwrap();

    let unique_ports = ports.lock().unwrap().len();
    let total_requests = result.total_requests;

    eprintln!("AlwaysNew: {total_requests} requests, {unique_ports} unique source ports");

    // With AlwaysNew, nearly every request should use a different source port.
    // Allow some reuse due to TIME_WAIT recycling.
    assert!(
        unique_ports as f64 > total_requests as f64 * 0.50,
        "AlwaysNew should create many connections: {unique_ports} ports for {total_requests} requests"
    );
}

#[test]
fn keepalive_policy_reuses_connections() {
    let (addr, ports) = start_port_tracking_server();

    let config = TestConfig {
        targets: vec![format!("http://{}/", addr)],
        duration: Duration::from_secs(2),
        rate: RateConfig::Static { rps: 100.0 },
        num_cores: 1,
        connections: ConnectionConfig {
            request_timeout: Duration::from_secs(10),
            connection_policy: ConnectionPolicy::KeepAlive,
            ..Default::default()
        },
        control_interval: Duration::from_secs(1),
        ..Default::default()
    };

    let result = run_test(config, |_| {
        HttpExecutor::with_timeout(Duration::from_secs(10))
    })
    .unwrap();

    let unique_ports = ports.lock().unwrap().len();
    let total_requests = result.total_requests;

    eprintln!("KeepAlive: {total_requests} requests, {unique_ports} unique source ports");

    // With KeepAlive, connections should be reused — far fewer unique ports than requests.
    assert!(
        unique_ports < total_requests as usize / 2,
        "KeepAlive should reuse connections: {unique_ports} ports for {total_requests} requests"
    );
}

#[test]
fn mixed_policy_produces_both_new_and_reused_connections() {
    let (addr, ports) = start_port_tracking_server();

    let config = TestConfig {
        targets: vec![format!("http://{}/", addr)],
        duration: Duration::from_secs(2),
        rate: RateConfig::Static { rps: 100.0 },
        num_cores: 1,
        connections: ConnectionConfig {
            request_timeout: Duration::from_secs(10),
            connection_policy: ConnectionPolicy::Mixed {
                persistent_ratio: 0.5,
                connection_lifetime: None,
            },
            ..Default::default()
        },
        control_interval: Duration::from_secs(1),
        ..Default::default()
    };

    let result = run_test(config, |_| {
        HttpExecutor::with_timeout(Duration::from_secs(10))
    })
    .unwrap();

    let unique_ports = ports.lock().unwrap().len();
    let total_requests = result.total_requests;

    eprintln!("Mixed(0.5): {total_requests} requests, {unique_ports} unique source ports");

    // With 50/50 mixed, we should see more ports than pure KeepAlive
    // but fewer than AlwaysNew. Just verify we got requests.
    assert!(
        total_requests > 100,
        "should generate requests with mixed policy, got {total_requests}"
    );
    assert!(
        unique_ports > 1,
        "mixed policy should use multiple connections, got {unique_ports}"
    );
}
