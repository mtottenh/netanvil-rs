//! Integration tests for the leader daemon API.
//!
//! Spins up a real leader API server on a random port and tests the
//! CRUD endpoints via raw HTTP.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use netanvil_distributed::leader_api::{self, LeaderApiState};
use netanvil_distributed::{LeaderMetricsState, ResultStore, TestQueue};

fn start_server() -> (String, Arc<LeaderApiState>) {
    let dir = tempfile::tempdir().unwrap();
    let store = ResultStore::open(dir.path(), 100).unwrap();
    let queue = TestQueue::new(store);

    let state = Arc::new(LeaderApiState {
        queue,
        agents: Arc::new(Mutex::new(Vec::new())),
        leader_metrics: LeaderMetricsState::new(),
    });

    // Find a free port by binding to :0.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    drop(listener);

    let bind = addr.clone();
    let server_state = state.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            leader_api::serve(&bind, server_state).await.unwrap();
        });
    });

    // Wait for the server to be ready.
    for _ in 0..50 {
        if TcpStream::connect(&addr).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // Keep the tempdir alive by leaking it (tests are short-lived).
    std::mem::forget(dir);

    (addr, state)
}

fn http_request(addr: &str, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    stream.set_write_timeout(Some(Duration::from_secs(5))).unwrap();

    let body_bytes = body.unwrap_or("");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body_bytes}",
        body_bytes.len()
    );
    stream.write_all(request.as_bytes()).unwrap();

    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();

    // Parse status code and body.
    let status_line = response.lines().next().unwrap_or("");
    let status_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();

    (status_code, body)
}

fn get(addr: &str, path: &str) -> (u16, serde_json::Value) {
    let (status, body) = http_request(addr, "GET", path, None);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    (status, json)
}

fn post(addr: &str, path: &str, body: &str) -> (u16, serde_json::Value) {
    let (status, resp) = http_request(addr, "POST", path, Some(body));
    let json: serde_json::Value = serde_json::from_str(&resp).unwrap_or_default();
    (status, json)
}

fn delete(addr: &str, path: &str) -> (u16, serde_json::Value) {
    let (status, body) = http_request(addr, "DELETE", path, None);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    (status, json)
}

fn valid_spec_json() -> String {
    r#"{
        "targets": ["http://localhost:9999"],
        "duration": "10s",
        "rate": {"type": "Static", "rps": 100}
    }"#
    .to_string()
}

#[test]
fn health_endpoint_returns_ok() {
    let (addr, _) = start_server();
    let (status, body) = get(&addr, "/health");
    assert_eq!(status, 200);
    assert_eq!(body["ok"], true);
}

#[test]
fn not_found_for_unknown_path() {
    let (addr, _) = start_server();
    let (status, body) = get(&addr, "/nonexistent");
    assert_eq!(status, 404);
    assert_eq!(body["ok"], false);
}

#[test]
fn create_test_returns_id_and_queued_status() {
    let (addr, _) = start_server();
    let (status, body) = post(&addr, "/tests", &valid_spec_json());
    assert_eq!(status, 200, "body: {body}");
    assert_eq!(body["ok"], true);
    assert!(body["test"]["id"].as_str().unwrap().starts_with("t-"));
    assert_eq!(body["test"]["status"], "queued");
}

#[test]
fn create_test_with_invalid_json_returns_400() {
    let (addr, _) = start_server();
    let (status, body) = post(&addr, "/tests", "not json");
    assert_eq!(status, 400);
    assert_eq!(body["ok"], false);
}

#[test]
fn create_test_with_invalid_duration_returns_400() {
    let (addr, _) = start_server();
    let (status, body) = post(
        &addr,
        "/tests",
        r#"{"targets": ["http://x"], "duration": "abc", "rate": {"type": "Static", "rps": 1}}"#,
    );
    assert_eq!(status, 400);
    assert_eq!(body["ok"], false);
}

#[test]
fn list_tests_includes_created_test() {
    let (addr, _) = start_server();
    let (_, create_resp) = post(&addr, "/tests", &valid_spec_json());
    let test_id = create_resp["test"]["id"].as_str().unwrap();

    let (status, list_resp) = get(&addr, "/tests");
    assert_eq!(status, 200);
    let tests = list_resp["tests"].as_array().unwrap();
    assert!(tests.iter().any(|t| t["id"].as_str() == Some(test_id)));
}

#[test]
fn list_tests_with_status_filter() {
    let (addr, _) = start_server();
    post(&addr, "/tests", &valid_spec_json());

    let (status, resp) = get(&addr, "/tests?status=queued");
    assert_eq!(status, 200);
    let tests = resp["tests"].as_array().unwrap();
    assert!(!tests.is_empty());
    assert!(tests.iter().all(|t| t["status"] == "queued"));

    let (_, resp) = get(&addr, "/tests?status=completed");
    let tests = resp["tests"].as_array().unwrap();
    assert!(tests.is_empty());
}

#[test]
fn list_tests_with_limit() {
    let (addr, _) = start_server();
    for _ in 0..5 {
        post(&addr, "/tests", &valid_spec_json());
    }

    let (_, resp) = get(&addr, "/tests?limit=2");
    let tests = resp["tests"].as_array().unwrap();
    assert_eq!(tests.len(), 2);
}

#[test]
fn get_test_by_id() {
    let (addr, _) = start_server();
    let (_, create_resp) = post(&addr, "/tests", &valid_spec_json());
    let test_id = create_resp["test"]["id"].as_str().unwrap();

    let (status, resp) = get(&addr, &format!("/tests/{test_id}"));
    assert_eq!(status, 200);
    assert_eq!(resp["id"].as_str(), Some(test_id));
    assert_eq!(resp["status"], "queued");
}

#[test]
fn get_test_not_found() {
    let (addr, _) = start_server();
    let (status, resp) = get(&addr, "/tests/t-nonexistent");
    assert_eq!(status, 404);
    assert_eq!(resp["ok"], false);
}

#[test]
fn get_result_before_completion_returns_404() {
    let (addr, _) = start_server();
    let (_, create_resp) = post(&addr, "/tests", &valid_spec_json());
    let test_id = create_resp["test"]["id"].as_str().unwrap();

    let (status, resp) = get(&addr, &format!("/tests/{test_id}/result"));
    assert_eq!(status, 404);
    assert_eq!(resp["ok"], false);
}

#[test]
fn cancel_queued_test() {
    let (addr, _) = start_server();
    let (_, create_resp) = post(&addr, "/tests", &valid_spec_json());
    let test_id = create_resp["test"]["id"].as_str().unwrap();

    let (status, resp) = delete(&addr, &format!("/tests/{test_id}"));
    assert_eq!(status, 200);
    assert_eq!(resp["ok"], true);
    assert_eq!(resp["action"], "cancelled");

    // Verify status updated.
    let (_, info) = get(&addr, &format!("/tests/{test_id}"));
    assert_eq!(info["status"], "cancelled");
}

#[test]
fn cancel_nonexistent_test_returns_404() {
    let (addr, _) = start_server();
    let (status, resp) = delete(&addr, "/tests/t-nonexistent");
    assert_eq!(status, 404);
    assert_eq!(resp["ok"], false);
}

#[test]
fn agents_endpoint_returns_empty_initially() {
    let (addr, _) = start_server();
    let (status, resp) = get(&addr, "/agents");
    assert_eq!(status, 200);
    let agents = resp["agents"].as_array().unwrap();
    assert!(agents.is_empty());
}

#[test]
fn put_rate_on_non_running_test_returns_409() {
    let (addr, _) = start_server();
    let (_, create_resp) = post(&addr, "/tests", &valid_spec_json());
    let test_id = create_resp["test"]["id"].as_str().unwrap();

    let (status, resp) = http_request(
        &addr,
        "PUT",
        &format!("/tests/{test_id}/rate"),
        Some(r#"{"rps": 500}"#),
    );
    let resp: serde_json::Value = serde_json::from_str(&resp).unwrap_or_default();
    assert_eq!(status, 409);
    assert_eq!(resp["ok"], false);
}

#[test]
fn put_signal_on_non_running_test_returns_409() {
    let (addr, _) = start_server();
    let (_, create_resp) = post(&addr, "/tests", &valid_spec_json());
    let test_id = create_resp["test"]["id"].as_str().unwrap();

    let (status, resp) = http_request(
        &addr,
        "PUT",
        &format!("/tests/{test_id}/signal"),
        Some(r#"{"name": "load", "value": 42.0}"#),
    );
    let resp: serde_json::Value = serde_json::from_str(&resp).unwrap_or_default();
    assert_eq!(status, 409);
    assert_eq!(resp["ok"], false);
}
