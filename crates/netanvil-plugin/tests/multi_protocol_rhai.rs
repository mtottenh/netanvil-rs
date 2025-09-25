//! Behavioral tests for Rhai plugin generation across all protocols.

use std::time::Instant;

use netanvil_plugin::RhaiGenerator;
use netanvil_types::{RequestContext, RequestGenerator};

fn mock_ctx(request_id: u64, core_id: usize) -> RequestContext {
    let now = Instant::now();
    RequestContext {
        request_id,
        intended_time: now,
        actual_time: now,
        core_id,
        is_sampled: false,
        session_id: None,
    }
}

// ─── HTTP ────────────────────────────────────────────────────────────────────

#[test]
fn rhai_http_basic() {
    let script = r#"
        fn generate(ctx) {
            #{
                method: "GET",
                url: "http://example.com/path?id=" + ctx.request_id,
                headers: [["X-Core", "" + ctx.core_id]],
                body: ()
            }
        }
    "#;

    let targets = vec!["http://example.com".into()];
    let mut gen =
        RhaiGenerator::<netanvil_types::HttpRequestSpec>::new(script, &targets).unwrap();

    let spec = gen.generate(&mock_ctx(42, 3));
    assert_eq!(spec.method, http::Method::GET);
    assert!(spec.url.contains("id=42"));
    assert_eq!(spec.headers.len(), 1);
    assert_eq!(spec.headers[0].0, "X-Core");
    assert_eq!(spec.headers[0].1, "3");
}

// ─── DNS ─────────────────────────────────────────────────────────────────────

#[test]
fn rhai_dns_basic() {
    let script = r#"
        fn generate(ctx) {
            #{
                query_name: "example.com",
                query_type: "AAAA",
                recursion: true,
                dnssec: false
            }
        }
    "#;

    let targets = vec![];
    let mut gen =
        RhaiGenerator::<netanvil_types::DnsRequestSpec>::new(script, &targets).unwrap();

    let spec = gen.generate(&mock_ctx(1, 0));
    assert_eq!(spec.query_name, "example.com");
    assert_eq!(spec.query_type, netanvil_types::DnsQueryType::AAAA);
    assert!(spec.recursion);
    assert!(!spec.dnssec);
}

#[test]
fn rhai_dns_minimal_defaults() {
    let script = r#"
        fn generate(ctx) {
            #{ query_name: "minimal.test" }
        }
    "#;

    let targets = vec![];
    let mut gen =
        RhaiGenerator::<netanvil_types::DnsRequestSpec>::new(script, &targets).unwrap();

    let spec = gen.generate(&mock_ctx(1, 0));
    assert_eq!(spec.query_name, "minimal.test");
    assert_eq!(spec.query_type, netanvil_types::DnsQueryType::A);
    assert!(spec.recursion);
}

// ─── TCP ─────────────────────────────────────────────────────────────────────

#[test]
fn rhai_tcp_payload() {
    let script = r#"
        fn generate(ctx) {
            #{ payload: "PING\r\n" }
        }
    "#;

    let targets = vec![];
    let mut gen =
        RhaiGenerator::<netanvil_types::TcpRequestSpec>::new(script, &targets).unwrap();

    let spec = gen.generate(&mock_ctx(1, 0));
    assert_eq!(spec.payload, b"PING\r\n");
}

#[test]
fn rhai_tcp_empty_payload() {
    let script = r#"
        fn generate(ctx) {
            #{}
        }
    "#;

    let targets = vec![];
    let mut gen =
        RhaiGenerator::<netanvil_types::TcpRequestSpec>::new(script, &targets).unwrap();

    let spec = gen.generate(&mock_ctx(1, 0));
    assert!(spec.payload.is_empty());
}

// ─── UDP ─────────────────────────────────────────────────────────────────────

#[test]
fn rhai_udp_payload() {
    let script = r#"
        fn generate(ctx) {
            #{ payload: "HELLO" }
        }
    "#;

    let targets = vec![];
    let mut gen =
        RhaiGenerator::<netanvil_types::UdpRequestSpec>::new(script, &targets).unwrap();

    let spec = gen.generate(&mock_ctx(1, 0));
    assert_eq!(spec.payload, b"HELLO");
}
