//! Behavioral tests for LuaJIT plugin generation across all protocols.
//!
//! Each test loads an inline Lua script, creates a protocol-specific generator,
//! calls generate() with a mock RequestContext, and asserts the returned spec.

use std::time::Instant;

use netanvil_plugin_luajit::LuaJitGenerator;
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
fn lua_http_basic_get() {
    let script = r#"
        function init(targets) end
        function generate(ctx)
            return {
                method = "GET",
                url = "http://example.com/path?id=" .. ctx.request_id,
                headers = {{"X-Core", tostring(ctx.core_id)}},
                body = nil,
            }
        end
    "#;

    let targets = vec!["http://example.com".into()];
    let mut gen = LuaJitGenerator::<netanvil_types::HttpRequestSpec>::new(script, &targets).unwrap();

    let spec = gen.generate(&mock_ctx(42, 3));
    assert_eq!(spec.method, http::Method::GET);
    assert!(spec.url.contains("id=42"));
    assert_eq!(spec.headers.len(), 1);
    assert_eq!(spec.headers[0], ("X-Core".into(), "3".into()));
    assert!(spec.body.is_none());
}

#[test]
fn lua_http_post_with_body() {
    let script = r#"
        function init(targets) end
        function generate(ctx)
            return {
                method = "POST",
                url = "http://api.example.com/items",
                headers = {{"Content-Type", "application/json"}},
                body = '{"id":' .. ctx.request_id .. '}',
            }
        end
    "#;

    let targets = vec!["http://api.example.com".into()];
    let mut gen = LuaJitGenerator::<netanvil_types::HttpRequestSpec>::new(script, &targets).unwrap();

    let spec = gen.generate(&mock_ctx(99, 0));
    assert_eq!(spec.method, http::Method::POST);
    assert_eq!(spec.body, Some(b"{\"id\":99}".to_vec()));
}

#[test]
fn lua_http_uses_init_targets() {
    let script = r#"
        local targets = {}
        function init(t) targets = t end
        function generate(ctx)
            return {
                method = "GET",
                url = targets[1] .. "/test",
                headers = {},
                body = nil,
            }
        end
    "#;

    let targets = vec!["http://myhost:8080".into()];
    let mut gen = LuaJitGenerator::<netanvil_types::HttpRequestSpec>::new(script, &targets).unwrap();

    let spec = gen.generate(&mock_ctx(1, 0));
    assert_eq!(spec.url, "http://myhost:8080/test");
}

// ─── DNS ─────────────────────────────────────────────────────────────────────

#[test]
fn lua_dns_basic_query() {
    let script = r#"
        function init(targets) end
        function generate(ctx)
            return {
                query_name = "example.com",
                query_type = "A",
                recursion = true,
                dnssec = false,
            }
        end
    "#;

    let targets = vec!["dns://8.8.8.8:53".into()];
    let mut gen = LuaJitGenerator::<netanvil_types::DnsRequestSpec>::new(script, &targets).unwrap();

    let spec = gen.generate(&mock_ctx(1, 0));
    assert_eq!(spec.query_name, "example.com");
    assert_eq!(spec.query_type, netanvil_types::DnsQueryType::A);
    assert!(spec.recursion);
    assert!(!spec.dnssec);
}

#[test]
fn lua_dns_varied_query_types() {
    let script = r#"
        local types = {"A", "AAAA", "MX", "TXT", "CNAME", "NS"}
        local counter = 0
        function init(targets) end
        function generate(ctx)
            counter = counter + 1
            local idx = ((counter - 1) % #types) + 1
            return {
                query_name = "test-" .. counter .. ".example.org",
                query_type = types[idx],
                recursion = true,
            }
        end
    "#;

    let targets = vec![];
    let mut gen = LuaJitGenerator::<netanvil_types::DnsRequestSpec>::new(script, &targets).unwrap();

    let spec1 = gen.generate(&mock_ctx(1, 0));
    assert_eq!(spec1.query_name, "test-1.example.org");
    assert_eq!(spec1.query_type, netanvil_types::DnsQueryType::A);

    let spec2 = gen.generate(&mock_ctx(2, 0));
    assert_eq!(spec2.query_name, "test-2.example.org");
    assert_eq!(spec2.query_type, netanvil_types::DnsQueryType::AAAA);

    let spec3 = gen.generate(&mock_ctx(3, 0));
    assert_eq!(spec3.query_type, netanvil_types::DnsQueryType::MX);

    let spec4 = gen.generate(&mock_ctx(4, 0));
    assert_eq!(spec4.query_type, netanvil_types::DnsQueryType::TXT);
}

#[test]
fn lua_dns_with_dnssec() {
    let script = r#"
        function init(targets) end
        function generate(ctx)
            return {
                query_name = "secure.example.com",
                query_type = "A",
                recursion = false,
                dnssec = true,
            }
        end
    "#;

    let targets = vec![];
    let mut gen = LuaJitGenerator::<netanvil_types::DnsRequestSpec>::new(script, &targets).unwrap();

    let spec = gen.generate(&mock_ctx(1, 0));
    assert_eq!(spec.query_name, "secure.example.com");
    assert!(!spec.recursion);
    assert!(spec.dnssec);
}

#[test]
fn lua_dns_defaults_to_a_record_with_recursion() {
    // When query_type and recursion are omitted, defaults should apply:
    // query_type → A, recursion → true.
    // This tests the nil-vs-false distinction for Lua booleans.
    let script = r#"
        function init(targets) end
        function generate(ctx)
            return { query_name = "minimal.example.com" }
        end
    "#;

    let targets = vec![];
    let mut gen = LuaJitGenerator::<netanvil_types::DnsRequestSpec>::new(script, &targets).unwrap();

    let spec = gen.generate(&mock_ctx(1, 0));
    assert_eq!(spec.query_name, "minimal.example.com");
    assert_eq!(spec.query_type, netanvil_types::DnsQueryType::A);
    assert!(
        spec.recursion,
        "recursion should default to true when omitted"
    );
    assert!(!spec.dnssec, "dnssec should default to false when omitted");
}

#[test]
fn lua_dns_explicit_false_recursion_honored() {
    // Explicit `recursion = false` should NOT be overridden by the default.
    let script = r#"
        function init(targets) end
        function generate(ctx)
            return { query_name = "no-recurse.example.com", recursion = false }
        end
    "#;

    let targets = vec![];
    let mut gen = LuaJitGenerator::<netanvil_types::DnsRequestSpec>::new(script, &targets).unwrap();

    let spec = gen.generate(&mock_ctx(1, 0));
    assert!(!spec.recursion, "explicit false must be honored");
}

// ─── TCP ─────────────────────────────────────────────────────────────────────

#[test]
fn lua_tcp_string_payload() {
    let script = r#"
        function init(targets) end
        function generate(ctx)
            return { payload = "PING\r\n" }
        end
    "#;

    let targets = vec!["tcp://localhost:6379".into()];
    let mut gen = LuaJitGenerator::<netanvil_types::TcpRequestSpec>::new(script, &targets).unwrap();

    let spec = gen.generate(&mock_ctx(1, 0));
    assert_eq!(spec.payload, b"PING\r\n");
}

#[test]
fn lua_tcp_dynamic_payload() {
    let script = r#"
        local counter = 0
        function init(targets) end
        function generate(ctx)
            counter = counter + 1
            return { payload = "CMD " .. counter .. "\r\n" }
        end
    "#;

    let targets = vec![];
    let mut gen = LuaJitGenerator::<netanvil_types::TcpRequestSpec>::new(script, &targets).unwrap();

    let spec1 = gen.generate(&mock_ctx(1, 0));
    assert_eq!(spec1.payload, b"CMD 1\r\n");

    let spec2 = gen.generate(&mock_ctx(2, 0));
    assert_eq!(spec2.payload, b"CMD 2\r\n");
}

#[test]
fn lua_tcp_empty_payload_fallback() {
    let script = r#"
        function init(targets) end
        function generate(ctx)
            return {}
        end
    "#;

    let targets = vec![];
    let mut gen = LuaJitGenerator::<netanvil_types::TcpRequestSpec>::new(script, &targets).unwrap();

    let spec = gen.generate(&mock_ctx(1, 0));
    assert!(spec.payload.is_empty());
}

// ─── UDP ─────────────────────────────────────────────────────────────────────

#[test]
fn lua_udp_string_payload() {
    let script = r#"
        function init(targets) end
        function generate(ctx)
            return { payload = "HELLO" }
        end
    "#;

    let targets = vec!["udp://localhost:9999".into()];
    let mut gen = LuaJitGenerator::<netanvil_types::UdpRequestSpec>::new(script, &targets).unwrap();

    let spec = gen.generate(&mock_ctx(1, 0));
    assert_eq!(spec.payload, b"HELLO");
}

#[test]
fn lua_udp_dynamic_payload() {
    let script = r#"
        local counter = 0
        function init(targets) end
        function generate(ctx)
            counter = counter + 1
            return { payload = string.format("SEQ:%04d", counter) }
        end
    "#;

    let targets = vec![];
    let mut gen = LuaJitGenerator::<netanvil_types::UdpRequestSpec>::new(script, &targets).unwrap();

    let spec1 = gen.generate(&mock_ctx(1, 0));
    assert_eq!(spec1.payload, b"SEQ:0001");

    let spec2 = gen.generate(&mock_ctx(2, 0));
    assert_eq!(spec2.payload, b"SEQ:0002");
}

// ─── Cross-protocol: same script structure, different Spec ───────────────────

#[test]
fn lua_context_fields_available_in_all_protocols() {
    // This script uses all ctx fields — verify they're passed correctly
    // regardless of which protocol spec we're generating.
    let script = r#"
        function init(targets) end
        function generate(ctx)
            assert(ctx.request_id ~= nil, "request_id missing")
            assert(ctx.core_id ~= nil, "core_id missing")
            assert(ctx.is_sampled ~= nil, "is_sampled missing")
            return {
                query_name = "ctx-check.example.com",
                query_type = "A",
            }
        end
    "#;

    let targets = vec![];
    let mut gen = LuaJitGenerator::<netanvil_types::DnsRequestSpec>::new(script, &targets).unwrap();

    // Should not panic — ctx fields are all present
    let spec = gen.generate(&mock_ctx(12345, 7));
    assert_eq!(spec.query_name, "ctx-check.example.com");
}

// ─── Example plugin: dns_enumeration.lua ─────────────────────────────────────

#[test]
fn lua_dns_enumeration_example_generates_valid_specs() {
    let script = include_str!("../../netanvil-plugin/examples/scripts/dns_enumeration.lua");

    let targets = vec!["dns://8.8.8.8:53".into()];
    let mut gen = LuaJitGenerator::<netanvil_types::DnsRequestSpec>::new(script, &targets).unwrap();

    // Generate several specs and verify they're well-formed
    for i in 1..=100 {
        let spec = gen.generate(&mock_ctx(i, 0));
        assert!(!spec.query_name.is_empty(), "empty query_name at i={i}");
        assert!(
            spec.query_name.contains('.'),
            "query_name '{}' should be a FQDN",
            spec.query_name
        );
        assert!(spec.recursion);
    }
}
