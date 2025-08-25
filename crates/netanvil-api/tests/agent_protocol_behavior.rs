//! Behavioral tests for TCP/UDP protocol integration with the agent server.
//!
//! Verifies that `POST /test/start` with `ProtocolConfig::Tcp` and
//! `ProtocolConfig::Udp` correctly dispatches to the appropriate executor,
//! and that metrics are collected and rate can be adjusted mid-test.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use netanvil_api::AgentServer;
use netanvil_types::{ConnectionConfig, ProtocolConfig, RateConfig, TestConfig};

// ---------------------------------------------------------------------------
// Simple HTTP client helpers (same pattern as api_behavior.rs)
// ---------------------------------------------------------------------------

fn http_get(addr: &str, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    parse_http_response(&response)
}

fn http_post_json(addr: &str, path: &str, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    parse_http_response(&response)
}

fn http_put(addr: &str, path: &str, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "PUT {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    parse_http_response(&response)
}

fn http_post(addr: &str, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    parse_http_response(&response)
}

fn parse_http_response(raw: &str) -> (u16, String) {
    let parts: Vec<&str> = raw.splitn(2, "\r\n\r\n").collect();
    let status_line = raw.lines().next().unwrap_or("");
    let status_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = parts.get(1).unwrap_or(&"").to_string();
    (status_code, body)
}

// ---------------------------------------------------------------------------
// Helper: start an agent on a given port, returning a join handle.
// The agent thread exits when the server is stopped.
// ---------------------------------------------------------------------------

fn start_agent(port: u16) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name(format!("agent-{port}"))
        .spawn(move || {
            let server = AgentServer::new(port, 1).expect("start agent");
            server.run();
        })
        .unwrap()
}

// ---------------------------------------------------------------------------
// Helper: build a TCP test config for the agent.
// ---------------------------------------------------------------------------

fn make_tcp_config(
    tcp_server_addr: std::net::SocketAddr,
    mode: &str,
    rps: f64,
    duration_secs: u64,
    payload_hex: &str,
    request_size: u16,
    response_size: u32,
) -> TestConfig {
    TestConfig {
        targets: vec![format!("tcp://{}", tcp_server_addr)],
        duration: Duration::from_secs(duration_secs),
        rate: RateConfig::Static { rps },
        num_cores: 1,
        error_status_threshold: 0,
        connections: ConnectionConfig {
            request_timeout: Duration::from_secs(5),
            ..Default::default()
        },
        metrics_interval: Duration::from_millis(200),
        control_interval: Duration::from_millis(100),
        protocol: Some(ProtocolConfig::Tcp {
            mode: mode.to_string(),
            payload_hex: payload_hex.to_string(),
            framing: "raw".to_string(),
            request_size,
            response_size,
        }),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Helper: poll metrics until total_requests > 0 or timeout.
// ---------------------------------------------------------------------------

fn poll_metrics_until_requests(api_addr: &str, timeout: Duration) -> serde_json::Value {
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > timeout {
            panic!("timed out waiting for metrics with requests > 0");
        }
        std::thread::sleep(Duration::from_millis(300));
        let (status, body) = http_get(api_addr, "/metrics");
        if status == 200 {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                if json["total_requests"].as_u64().unwrap_or(0) > 0 {
                    return json;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn agent_runs_tcp_rr_test() {
    let tcp_server = netanvil_test_servers::tcp::start_tcp_echo();
    let agent_port = 18090;
    let _agent = start_agent(agent_port);
    std::thread::sleep(Duration::from_millis(300));

    let config = make_tcp_config(
        tcp_server.addr,
        "rr",
        50.0,
        3,
        "48454c4c4f", // "HELLO"
        5,
        5,
    );

    let config_json = serde_json::to_string(&config).unwrap();
    let api_addr = format!("127.0.0.1:{agent_port}");

    let (status, _body) = http_post_json(&api_addr, "/test/start", &config_json);
    assert_eq!(status, 200, "expected 200 from /test/start");

    // Wait for the test to generate some data, then poll
    let metrics = poll_metrics_until_requests(&api_addr, Duration::from_secs(8));

    let total = metrics["total_requests"].as_u64().unwrap_or(0);
    assert!(
        total > 0,
        "expected agent to make TCP RR requests, got {total}"
    );

    // Wait for the test to finish
    std::thread::sleep(Duration::from_secs(3));

    // Stop the agent
    let _ = http_post(&api_addr, "/stop");
}

#[test]
fn agent_runs_tcp_sink_test() {
    let tcp_server = netanvil_test_servers::tcp::start_tcp_echo();
    let agent_port = 18091;
    let _agent = start_agent(agent_port);
    std::thread::sleep(Duration::from_millis(300));

    let config = TestConfig {
        targets: vec![format!("tcp://{}", tcp_server.addr)],
        duration: Duration::from_secs(3),
        rate: RateConfig::Static { rps: 100.0 },
        num_cores: 1,
        error_status_threshold: 0,
        connections: ConnectionConfig {
            request_timeout: Duration::from_secs(5),
            ..Default::default()
        },
        metrics_interval: Duration::from_millis(200),
        control_interval: Duration::from_millis(100),
        protocol: Some(ProtocolConfig::Tcp {
            mode: "sink".to_string(),
            payload_hex: "AABBCCDD".to_string(),
            framing: "raw".to_string(),
            request_size: 1024,
            response_size: 0,
        }),
        ..Default::default()
    };

    let config_json = serde_json::to_string(&config).unwrap();
    let api_addr = format!("127.0.0.1:{agent_port}");

    let (status, _body) = http_post_json(&api_addr, "/test/start", &config_json);
    assert_eq!(status, 200, "expected 200 from /test/start for sink mode");

    let metrics = poll_metrics_until_requests(&api_addr, Duration::from_secs(8));
    let total = metrics["total_requests"].as_u64().unwrap_or(0);
    assert!(
        total > 0,
        "expected agent to make TCP SINK requests, got {total}"
    );

    std::thread::sleep(Duration::from_secs(3));
    let _ = http_post(&api_addr, "/stop");
}

#[test]
fn agent_runs_tcp_source_test() {
    let tcp_server = netanvil_test_servers::tcp::start_tcp_echo();
    let agent_port = 18092;
    let _agent = start_agent(agent_port);
    std::thread::sleep(Duration::from_millis(300));

    let config = TestConfig {
        targets: vec![format!("tcp://{}", tcp_server.addr)],
        duration: Duration::from_secs(3),
        rate: RateConfig::Static { rps: 100.0 },
        num_cores: 1,
        error_status_threshold: 0,
        connections: ConnectionConfig {
            request_timeout: Duration::from_secs(5),
            ..Default::default()
        },
        metrics_interval: Duration::from_millis(200),
        control_interval: Duration::from_millis(100),
        protocol: Some(ProtocolConfig::Tcp {
            mode: "source".to_string(),
            payload_hex: "".to_string(),
            framing: "raw".to_string(),
            request_size: 0,
            response_size: 1024,
        }),
        ..Default::default()
    };

    let config_json = serde_json::to_string(&config).unwrap();
    let api_addr = format!("127.0.0.1:{agent_port}");

    let (status, _body) = http_post_json(&api_addr, "/test/start", &config_json);
    assert_eq!(status, 200, "expected 200 from /test/start for source mode");

    let metrics = poll_metrics_until_requests(&api_addr, Duration::from_secs(8));
    let total = metrics["total_requests"].as_u64().unwrap_or(0);
    assert!(
        total > 0,
        "expected agent to make TCP SOURCE requests, got {total}"
    );

    std::thread::sleep(Duration::from_secs(3));
    let _ = http_post(&api_addr, "/stop");
}

#[test]
fn agent_runs_udp_test() {
    let udp_server = netanvil_test_servers::udp::start_udp_echo();
    let agent_port = 18093;
    let _agent = start_agent(agent_port);
    std::thread::sleep(Duration::from_millis(300));

    let config = TestConfig {
        targets: vec![format!("udp://{}", udp_server.addr)],
        duration: Duration::from_secs(3),
        rate: RateConfig::Static { rps: 50.0 },
        num_cores: 1,
        error_status_threshold: 0,
        connections: ConnectionConfig {
            request_timeout: Duration::from_secs(5),
            ..Default::default()
        },
        metrics_interval: Duration::from_millis(200),
        control_interval: Duration::from_millis(100),
        protocol: Some(ProtocolConfig::Udp {
            payload_hex: "48454c4c4f".to_string(), // "HELLO"
            expect_response: true,
        }),
        ..Default::default()
    };

    let config_json = serde_json::to_string(&config).unwrap();
    let api_addr = format!("127.0.0.1:{agent_port}");

    let (status, _body) = http_post_json(&api_addr, "/test/start", &config_json);
    assert_eq!(status, 200, "expected 200 from /test/start for UDP test");

    let metrics = poll_metrics_until_requests(&api_addr, Duration::from_secs(8));
    let total = metrics["total_requests"].as_u64().unwrap_or(0);
    assert!(
        total > 0,
        "expected agent to make UDP requests, got {total}"
    );

    std::thread::sleep(Duration::from_secs(3));
    let _ = http_post(&api_addr, "/stop");
}

#[test]
fn agent_tcp_rate_control() {
    let tcp_server = netanvil_test_servers::tcp::start_tcp_echo();
    let agent_port = 18094;
    let _agent = start_agent(agent_port);
    std::thread::sleep(Duration::from_millis(300));

    let config = make_tcp_config(
        tcp_server.addr,
        "rr",
        50.0,
        6,
        "50494e47", // "PING"
        4,
        4,
    );

    let config_json = serde_json::to_string(&config).unwrap();
    let api_addr = format!("127.0.0.1:{agent_port}");

    let (status, _body) = http_post_json(&api_addr, "/test/start", &config_json);
    assert_eq!(status, 200, "expected 200 from /test/start");

    // Wait for the test to produce initial metrics at 50 RPS
    let _initial = poll_metrics_until_requests(&api_addr, Duration::from_secs(5));

    // Increase rate to 200 RPS
    let (put_status, put_body) = http_put(&api_addr, "/rate", r#"{"rps": 200.0}"#);
    assert_eq!(put_status, 200);
    let put_json: serde_json::Value = serde_json::from_str(&put_body).unwrap();
    assert_eq!(put_json["ok"], true, "PUT /rate should succeed");

    // Wait for the rate change to propagate
    std::thread::sleep(Duration::from_millis(1000));

    // Verify the target RPS increased
    let (status, body) = http_get(&api_addr, "/status");
    assert_eq!(status, 200);
    let status_json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let target = status_json["target_rps"].as_f64().unwrap_or(0.0);
    assert!(
        target > 100.0,
        "target RPS should reflect rate increase, got {target}"
    );

    // Wait for test to finish, then stop agent
    std::thread::sleep(Duration::from_secs(5));
    let _ = http_post(&api_addr, "/stop");
}
