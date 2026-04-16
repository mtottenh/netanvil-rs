//! Integration tests for the `netanvil-test-server` binary.
//!
//! These tests spawn the actual binary as a subprocess, parse its stderr
//! for listening addresses, exercise the protocols, and validate behavior.
//! Tests cover subcommand dispatch, multi-protocol mode, symlink detection,
//! and JSON reporting.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, UdpSocket};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Find the built binary. Cargo puts it in target/debug/ during tests.
fn binary_path() -> std::path::PathBuf {
    let mut path = std::env::current_exe()
        .expect("current_exe")
        .parent()
        .expect("parent of test binary")
        .parent()
        .expect("parent of deps/")
        .to_path_buf();
    path.push("netanvil-test-server");
    assert!(
        path.exists(),
        "binary not found at {:?} — run `cargo build -p netanvil-test-servers` first",
        path
    );
    path
}

/// Parse "TCP test server listening on 127.0.0.1:<port>" from a stderr line.
fn parse_listen_addr(line: &str, protocol: &str) -> Option<SocketAddr> {
    let prefix = format!("{} test server listening on ", protocol);
    if let Some(rest) = line.strip_prefix(&prefix) {
        rest.trim().parse().ok()
    } else {
        None
    }
}

/// Spawn the binary with given args and wait for "Press Ctrl+C" on stderr.
/// Returns (child, tcp_addr, udp_addr, dns_addr).
fn spawn_and_wait(
    args: &[&str],
) -> (
    std::process::Child,
    Option<SocketAddr>,
    Option<SocketAddr>,
    Option<SocketAddr>,
) {
    let mut cmd = Command::new(binary_path());
    cmd.args(args);
    cmd.stderr(Stdio::piped());
    cmd.stdout(Stdio::null());

    let mut child = cmd.spawn().expect("failed to spawn netanvil-test-server");
    let stderr = child.stderr.take().expect("stderr");
    let reader = BufReader::new(stderr);

    let mut tcp_addr = None;
    let mut udp_addr = None;
    let mut dns_addr = None;

    for line in reader.lines() {
        let line = line.expect("readline");
        if let Some(addr) = parse_listen_addr(&line, "TCP") {
            tcp_addr = Some(addr);
        }
        if let Some(addr) = parse_listen_addr(&line, "UDP") {
            udp_addr = Some(addr);
        }
        if let Some(addr) = parse_listen_addr(&line, "DNS") {
            dns_addr = Some(addr);
        }
        if line.contains("Press Ctrl+C") {
            break;
        }
    }

    (child, tcp_addr, udp_addr, dns_addr)
}

// ---------------------------------------------------------------------------
// Subcommand tests
// ---------------------------------------------------------------------------

#[test]
fn test_tcp_subcommand_rr_transaction() {
    let (mut child, tcp_addr, udp_addr, _) = spawn_and_wait(&[
        "tcp",
        "--port",
        "0",
        "--workers",
        "1",
        "--pin-cores",
        "false",
    ]);

    assert!(tcp_addr.is_some(), "TCP addr should be reported");
    assert!(udp_addr.is_none(), "UDP should NOT be started in tcp mode");

    let tcp_addr = tcp_addr.unwrap();
    let mut stream =
        std::net::TcpStream::connect_timeout(&tcp_addr, Duration::from_secs(2)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();

    // v2 header: RR mode, request_size=32, response_size=64
    let header: [u8; 10] = [0xF0, 10, 0x01, 0x00, 0x00, 0x20, 0x00, 0x00, 0x00, 0x40];
    stream.write_all(&header).unwrap();
    stream.write_all(&[0xAB; 32]).unwrap();

    let mut response = [0u8; 64];
    stream.read_exact(&mut response).unwrap();

    let nonzero = response.iter().filter(|&&b| b != 0).count();
    assert!(
        nonzero > 40,
        "response should be PRNG-filled, got {} non-zero bytes out of 64",
        nonzero
    );

    drop(stream);
    child.kill().ok();
    child.wait().ok();
}

#[test]
fn test_udp_subcommand_echo() {
    let (mut child, tcp_addr, udp_addr, _) = spawn_and_wait(&[
        "udp",
        "--port",
        "0",
        "--workers",
        "1",
        "--pin-cores",
        "false",
    ]);

    assert!(tcp_addr.is_none(), "TCP should NOT be started in udp mode");
    assert!(udp_addr.is_some(), "UDP addr should be reported");

    let udp_addr = udp_addr.unwrap();
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();

    let payload = b"hello from udp subcommand test";
    socket.send_to(payload, udp_addr).unwrap();

    let mut buf = [0u8; 256];
    let (n, _) = socket.recv_from(&mut buf).unwrap();

    assert_eq!(&buf[..n], payload, "UDP echo should return same payload");

    child.kill().ok();
    child.wait().ok();
}

#[test]
fn test_dns_subcommand_starts() {
    let (mut child, tcp_addr, udp_addr, dns_addr) = spawn_and_wait(&[
        "dns",
        "--port",
        "0",
        "--workers",
        "1",
        "--pin-cores",
        "false",
    ]);

    assert!(tcp_addr.is_none(), "TCP should NOT be started in dns mode");
    assert!(udp_addr.is_none(), "UDP should NOT be started in dns mode");
    assert!(dns_addr.is_some(), "DNS addr should be reported");

    child.kill().ok();
    child.wait().ok();
}

#[test]
fn test_multi_subcommand() {
    let (mut child, tcp_addr, udp_addr, _) = spawn_and_wait(&[
        "multi",
        "--tcp-port",
        "0",
        "--udp-port",
        "0",
        "--workers",
        "1",
        "--pin-cores",
        "false",
    ]);

    assert!(tcp_addr.is_some(), "TCP should be started in multi mode");
    assert!(udp_addr.is_some(), "UDP should be started in multi mode");

    // Quick TCP sanity check
    let tcp_addr = tcp_addr.unwrap();
    let stream = std::net::TcpStream::connect_timeout(&tcp_addr, Duration::from_secs(2));
    assert!(stream.is_ok(), "should be able to connect to TCP port");

    // Quick UDP sanity check
    let udp_addr = udp_addr.unwrap();
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    socket.send_to(b"ping", udp_addr).unwrap();
    let mut buf = [0u8; 64];
    let (n, _) = socket.recv_from(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"ping");

    child.kill().ok();
    child.wait().ok();
}

// ---------------------------------------------------------------------------
// CRR mode via subcommand
// ---------------------------------------------------------------------------

#[test]
fn test_tcp_crr_via_subcommand() {
    let (mut child, tcp_addr, _, _) = spawn_and_wait(&[
        "tcp",
        "--port",
        "0",
        "--workers",
        "1",
        "--pin-cores",
        "false",
    ]);
    let tcp_addr = tcp_addr.unwrap();

    for _ in 0..5 {
        let mut stream =
            std::net::TcpStream::connect_timeout(&tcp_addr, Duration::from_secs(2)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();

        // v2 CRR header: request_size=16, response_size=32
        let header: [u8; 10] = [0xF0, 10, 0x05, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x20];
        stream.write_all(&header).unwrap();
        stream.write_all(&[0xCC; 16]).unwrap();

        let mut response = [0u8; 32];
        stream.read_exact(&mut response).unwrap();

        // Server should close after CRR
        let mut extra = [0u8; 1];
        match stream.read(&mut extra) {
            Ok(0) | Err(_) => {} // EOF or reset — expected
            Ok(n) => panic!("expected EOF after CRR, got {} bytes", n),
        }
    }

    child.kill().ok();
    child.wait().ok();
}

// ---------------------------------------------------------------------------
// Symlink detection
// ---------------------------------------------------------------------------

#[test]
fn test_symlink_tcp_echo() {
    // Create a symlink named netanvil-tcp-echo pointing to the binary.
    let bin = binary_path();
    let symlink_dir = std::env::temp_dir().join("netanvil-test-symlinks");
    let _ = std::fs::create_dir_all(&symlink_dir);
    let symlink_path = symlink_dir.join("netanvil-tcp-echo");
    let _ = std::fs::remove_file(&symlink_path); // clean up from prior run

    #[cfg(unix)]
    std::os::unix::fs::symlink(&bin, &symlink_path).expect("failed to create symlink");

    #[cfg(not(unix))]
    {
        eprintln!("symlink test skipped on non-unix");
        return;
    }

    // Invoke via symlink — should parse as TcpArgs (no subcommand needed).
    let mut cmd = Command::new(&symlink_path);
    cmd.args(["--port", "0", "--workers", "1", "--pin-cores", "false"]);
    cmd.stderr(Stdio::piped());
    cmd.stdout(Stdio::null());

    let mut child = cmd.spawn().expect("failed to spawn via symlink");
    let stderr = child.stderr.take().unwrap();
    let reader = BufReader::new(stderr);

    let mut tcp_addr = None;
    for line in reader.lines() {
        let line = line.expect("readline");
        if let Some(addr) = parse_listen_addr(&line, "TCP") {
            tcp_addr = Some(addr);
        }
        if line.contains("Press Ctrl+C") {
            break;
        }
    }

    assert!(
        tcp_addr.is_some(),
        "symlink invocation should start TCP server"
    );

    // Verify we can connect
    let stream = std::net::TcpStream::connect_timeout(&tcp_addr.unwrap(), Duration::from_secs(2));
    assert!(
        stream.is_ok(),
        "should connect to symlink-started TCP server"
    );

    child.kill().ok();
    child.wait().ok();
    let _ = std::fs::remove_file(&symlink_path);
}

// ---------------------------------------------------------------------------
// JSON reporting
// ---------------------------------------------------------------------------

#[test]
fn test_json_reporting_via_subcommand() {
    let mut cmd = Command::new(binary_path());
    cmd.args([
        "tcp",
        "--port",
        "0",
        "--workers",
        "1",
        "--pin-cores",
        "false",
        "--report",
        "json",
        "--report-interval",
        "0.5",
    ]);
    cmd.stderr(Stdio::piped());
    cmd.stdout(Stdio::null());

    let mut child = cmd.spawn().expect("failed to spawn");
    let stderr = child.stderr.take().unwrap();
    let mut reader = BufReader::new(stderr);

    // Read until ready.
    let mut tcp_addr = None;
    let mut line = String::new();
    loop {
        line.clear();
        reader.read_line(&mut line).expect("readline");
        if let Some(addr) = parse_listen_addr(line.trim(), "TCP") {
            tcp_addr = Some(addr);
        }
        if line.contains("Press Ctrl+C") {
            break;
        }
    }
    let tcp_addr = tcp_addr.expect("TCP addr");

    // Generate traffic so JSON has non-zero data.
    let mut stream =
        std::net::TcpStream::connect_timeout(&tcp_addr, Duration::from_secs(2)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();

    // v2 RR header: request_size=8, response_size=16
    let header: [u8; 10] = [0xF0, 10, 0x01, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x10];
    stream.write_all(&header).unwrap();
    for _ in 0..10 {
        stream.write_all(&[0xAA; 8]).unwrap();
        let mut resp = [0u8; 16];
        stream.read_exact(&mut resp).unwrap();
    }
    drop(stream);

    // Wait for at least one JSON report.
    std::thread::sleep(Duration::from_millis(1500));
    child.kill().ok();

    // Read remaining stderr.
    let mut remaining = String::new();
    reader.read_to_string(&mut remaining).ok();

    let json_lines: Vec<&str> = remaining.lines().filter(|l| l.starts_with('{')).collect();
    assert!(
        !json_lines.is_empty(),
        "expected JSON report lines, got none. stderr tail: {}",
        &remaining[..remaining.len().min(500)]
    );

    let last = json_lines.last().unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(last).unwrap_or_else(|e| panic!("invalid JSON: {}: {}", e, last));

    assert!(parsed.get("bytes_sent").is_some(), "missing bytes_sent");
    assert!(
        parsed.get("bytes_received").is_some(),
        "missing bytes_received"
    );
    assert!(parsed.get("workers").is_some(), "missing workers array");

    child.wait().ok();
}

// ---------------------------------------------------------------------------
// --help scoping
// ---------------------------------------------------------------------------

#[test]
fn test_help_shows_subcommands() {
    let output = Command::new(binary_path())
        .arg("--help")
        .output()
        .expect("failed to run --help");

    assert!(output.status.success(), "--help should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("tcp"), "should list tcp subcommand");
    assert!(stdout.contains("udp"), "should list udp subcommand");
    assert!(stdout.contains("dns"), "should list dns subcommand");
    assert!(stdout.contains("multi"), "should list multi subcommand");
}

#[test]
fn test_tcp_help_shows_only_tcp_flags() {
    let output = Command::new(binary_path())
        .args(["tcp", "--help"])
        .output()
        .expect("failed to run tcp --help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    // TCP flags should be present
    assert!(
        stdout.contains("--tcp-health-sample-rate"),
        "tcp --help should show --tcp-health-sample-rate"
    );
    assert!(
        stdout.contains("--max-connections"),
        "tcp --help should show --max-connections"
    );

    // UDP flags should NOT be present
    assert!(
        !stdout.contains("--udp-pacing-bps"),
        "tcp --help should NOT show --udp-pacing-bps"
    );
    assert!(
        !stdout.contains("--udp-drop-rate"),
        "tcp --help should NOT show --udp-drop-rate"
    );
    assert!(
        !stdout.contains("--udp-idle-timeout"),
        "tcp --help should NOT show --udp-idle-timeout"
    );
}

#[test]
fn test_udp_help_shows_only_udp_flags() {
    let output = Command::new(binary_path())
        .args(["udp", "--help"])
        .output()
        .expect("failed to run udp --help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    // UDP flags should be present
    assert!(
        stdout.contains("--udp-pacing-bps"),
        "udp --help should show --udp-pacing-bps"
    );
    assert!(
        stdout.contains("--udp-drop-rate"),
        "udp --help should show --udp-drop-rate"
    );

    // TCP flags should NOT be present
    assert!(
        !stdout.contains("--tcp-health-sample-rate"),
        "udp --help should NOT show --tcp-health-sample-rate"
    );
    assert!(
        !stdout.contains("--max-connections"),
        "udp --help should NOT show --max-connections"
    );
}
