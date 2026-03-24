use std::time::Duration;

use netanvil_core::GenericTestBuilder;
use netanvil_dns::{
    DnsExecutor, DnsNoopTransformer, DnsQueryType, DnsRequestSpec, SimpleDnsGenerator,
};
use netanvil_types::{RateConfig, RequestGenerator, RequestTransformer, TestConfig};

#[test]
fn test_dns_query_a_record() {
    let server = netanvil_test_servers::dns::start_dns_echo();
    let servers = vec![server.addr];
    let domains = vec!["example.com".to_string()];

    let config = TestConfig {
        targets: vec![format!("{}", server.addr)],
        duration: Duration::from_secs(2),
        rate: RateConfig::Static { rps: 50.0 },
        num_cores: 1,
        error_status_threshold: 0,
        ..Default::default()
    };

    let result = GenericTestBuilder::new(
        config,
        |_| DnsExecutor::with_timeout(Duration::from_secs(5)),
        Box::new(move |_| {
            Box::new(SimpleDnsGenerator::new(
                servers.clone(),
                domains.clone(),
                DnsQueryType::A,
                true,
            )) as Box<dyn RequestGenerator<Spec = DnsRequestSpec>>
        }),
        Box::new(|_| {
            Box::new(DnsNoopTransformer) as Box<dyn RequestTransformer<Spec = DnsRequestSpec>>
        }),
    )
    .run()
    .expect("test run failed");

    assert!(result.total_requests > 0, "expected some requests, got 0");
    assert_eq!(
        result.total_errors, 0,
        "expected 0 errors, got {}",
        result.total_errors
    );
}

#[test]
fn test_dns_multiple_domains() {
    let server = netanvil_test_servers::dns::start_dns_echo();
    let servers = vec![server.addr];
    let domains = vec![
        "example.com".to_string(),
        "google.com".to_string(),
        "github.com".to_string(),
    ];

    let config = TestConfig {
        targets: vec![format!("{}", server.addr)],
        duration: Duration::from_secs(2),
        rate: RateConfig::Static { rps: 100.0 },
        num_cores: 1,
        error_status_threshold: 0,
        ..Default::default()
    };

    let result = GenericTestBuilder::new(
        config,
        |_| DnsExecutor::with_timeout(Duration::from_secs(5)),
        Box::new(move |_| {
            Box::new(SimpleDnsGenerator::new(
                servers.clone(),
                domains.clone(),
                DnsQueryType::A,
                true,
            )) as Box<dyn RequestGenerator<Spec = DnsRequestSpec>>
        }),
        Box::new(|_| {
            Box::new(DnsNoopTransformer) as Box<dyn RequestTransformer<Spec = DnsRequestSpec>>
        }),
    )
    .run()
    .expect("test run failed");

    assert!(result.total_requests > 0);
    assert_eq!(result.total_errors, 0);
}

#[test]
fn test_dns_no_server_timeout() {
    // Target a port with no server — all queries should timeout
    let config = TestConfig {
        targets: vec!["127.0.0.1:19999".to_string()],
        duration: Duration::from_secs(2),
        rate: RateConfig::Static { rps: 10.0 },
        num_cores: 1,
        error_status_threshold: 0,
        ..Default::default()
    };

    let result = GenericTestBuilder::new(
        config,
        |_| DnsExecutor::with_timeout(Duration::from_millis(200)),
        Box::new(|_| {
            Box::new(SimpleDnsGenerator::new(
                vec!["127.0.0.1:19999".parse().unwrap()],
                vec!["example.com".to_string()],
                DnsQueryType::A,
                true,
            )) as Box<dyn RequestGenerator<Spec = DnsRequestSpec>>
        }),
        Box::new(|_| {
            Box::new(DnsNoopTransformer) as Box<dyn RequestTransformer<Spec = DnsRequestSpec>>
        }),
    )
    .run()
    .expect("test run failed");

    // All requests should have timed out (counted as errors since error_status_threshold=0
    // doesn't classify status-based errors, but timeout IS an error)
    assert!(result.total_requests > 0);
    assert!(
        result.total_errors > 0,
        "expected timeout errors, got 0 errors"
    );
}

#[test]
fn test_dns_lua_plugin() {
    let server = netanvil_test_servers::dns::start_dns_echo();

    let script = r#"
        function generate(ctx)
            return {
                query_name = "user" .. ctx.request_id .. ".example.com",
                query_type = "A",
                recursion = true
            }
        end
    "#;

    let script_owned = script.to_string();
    let server_addr = server.addr;

    let config = TestConfig {
        targets: vec![format!("{}", server.addr)],
        duration: Duration::from_secs(2),
        rate: RateConfig::Static { rps: 50.0 },
        num_cores: 1,
        error_status_threshold: 0,
        ..Default::default()
    };

    let result = GenericTestBuilder::new(
        config,
        |_| DnsExecutor::with_timeout(Duration::from_secs(5)),
        Box::new(move |_| {
            use netanvil_plugin_luajit::LuaJitGenerator;

            let targets = vec![format!("{}", server_addr)];

            // Wrap to inject the real server address since the plugin returns 0.0.0.0:0
            struct DnsPluginWrapper {
                inner: LuaJitGenerator<DnsRequestSpec>,
                server: std::net::SocketAddr,
            }

            impl RequestGenerator for DnsPluginWrapper {
                type Spec = DnsRequestSpec;

                fn generate(&mut self, ctx: &netanvil_types::RequestContext) -> DnsRequestSpec {
                    let mut spec = self.inner.generate(ctx);
                    spec.server = self.server;
                    spec
                }
            }

            Box::new(DnsPluginWrapper {
                inner: LuaJitGenerator::new(&script_owned, &targets).expect("LuaJIT init failed"),
                server: server_addr,
            }) as Box<dyn RequestGenerator<Spec = DnsRequestSpec>>
        }),
        Box::new(|_| {
            Box::new(DnsNoopTransformer) as Box<dyn RequestTransformer<Spec = DnsRequestSpec>>
        }),
    )
    .run()
    .expect("test run failed");

    assert!(
        result.total_requests > 0,
        "expected some requests from Lua plugin, got 0"
    );
    assert_eq!(
        result.total_errors, 0,
        "expected 0 errors with Lua DNS plugin, got {}",
        result.total_errors
    );
}
