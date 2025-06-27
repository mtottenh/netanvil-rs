//! Behavioral tests for the ConnectionPolicyTransformer.
//!
//! Validates that connection lifecycle policies correctly add or omit
//! the `Connection: close` header based on the configured policy.

use netanvil_core::{ConnectionPolicyTransformer, NoopTransformer};
use netanvil_types::{
    ConnectionPolicy, CountDistribution, RequestContext, RequestSpec, RequestTransformer,
};
use std::time::Instant;

fn make_context() -> RequestContext {
    let now = Instant::now();
    RequestContext {
        request_id: 1,
        intended_time: now,
        actual_time: now,
        core_id: 0,
        is_sampled: false,
        session_id: None,
    }
}

fn make_spec() -> RequestSpec {
    RequestSpec {
        method: http::Method::GET,
        url: "http://test.local/".into(),
        headers: vec![],
        body: None,
    }
}

fn has_connection_close(spec: &RequestSpec) -> bool {
    spec.headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("connection") && v.eq_ignore_ascii_case("close"))
}

#[test]
fn keepalive_policy_never_adds_connection_close() {
    let transformer =
        ConnectionPolicyTransformer::new(Box::new(NoopTransformer), ConnectionPolicy::KeepAlive);

    let ctx = make_context();
    for _ in 0..100 {
        let spec = transformer.transform(make_spec(), &ctx);
        assert!(
            !has_connection_close(&spec),
            "KeepAlive should never add Connection: close"
        );
    }
}

#[test]
fn always_new_policy_always_adds_connection_close() {
    let transformer =
        ConnectionPolicyTransformer::new(Box::new(NoopTransformer), ConnectionPolicy::AlwaysNew);

    let ctx = make_context();
    for _ in 0..100 {
        let spec = transformer.transform(make_spec(), &ctx);
        assert!(
            has_connection_close(&spec),
            "AlwaysNew should always add Connection: close"
        );
    }
}

#[test]
fn mixed_policy_respects_persistent_ratio() {
    // With persistent_ratio=0.5, approximately half should get Connection: close
    let transformer = ConnectionPolicyTransformer::new(
        Box::new(NoopTransformer),
        ConnectionPolicy::Mixed {
            persistent_ratio: 0.5,
            connection_lifetime: None,
        },
    );

    let ctx = make_context();
    let n = 1000;
    let mut close_count = 0;
    for _ in 0..n {
        let spec = transformer.transform(make_spec(), &ctx);
        if has_connection_close(&spec) {
            close_count += 1;
        }
    }

    // Expect ~500 ± 100 (statistical test)
    let ratio = close_count as f64 / n as f64;
    assert!(
        (0.35..=0.65).contains(&ratio),
        "mixed 50/50 should produce ~50% Connection: close, got {:.1}% ({close_count}/{n})",
        ratio * 100.0
    );
}

#[test]
fn mixed_policy_all_persistent_never_closes() {
    let transformer = ConnectionPolicyTransformer::new(
        Box::new(NoopTransformer),
        ConnectionPolicy::Mixed {
            persistent_ratio: 1.0,
            connection_lifetime: None,
        },
    );

    let ctx = make_context();
    for _ in 0..100 {
        let spec = transformer.transform(make_spec(), &ctx);
        assert!(
            !has_connection_close(&spec),
            "mixed with persistent_ratio=1.0 should never close"
        );
    }
}

#[test]
fn mixed_policy_zero_persistent_always_closes() {
    let transformer = ConnectionPolicyTransformer::new(
        Box::new(NoopTransformer),
        ConnectionPolicy::Mixed {
            persistent_ratio: 0.0,
            connection_lifetime: None,
        },
    );

    let ctx = make_context();
    for _ in 0..100 {
        let spec = transformer.transform(make_spec(), &ctx);
        assert!(
            has_connection_close(&spec),
            "mixed with persistent_ratio=0.0 should always close"
        );
    }
}

#[test]
fn fixed_lifetime_forces_close_at_boundary() {
    // persistent_ratio=1.0 (never random close) but lifetime=Fixed(10)
    // Every 10th request should force a close.
    let transformer = ConnectionPolicyTransformer::new(
        Box::new(NoopTransformer),
        ConnectionPolicy::Mixed {
            persistent_ratio: 1.0,
            connection_lifetime: Some(CountDistribution::Fixed(10)),
        },
    );

    let ctx = make_context();
    let mut close_indices = Vec::new();
    for i in 0..50 {
        let spec = transformer.transform(make_spec(), &ctx);
        if has_connection_close(&spec) {
            close_indices.push(i);
        }
    }

    // Requests 10, 20, 30, 40 should close (at the boundary)
    // Request 0 starts a fresh connection with lifetime=10, so close at request 10
    assert_eq!(
        close_indices,
        vec![10, 20, 30, 40],
        "should close every 10 requests, closed at: {close_indices:?}"
    );
}

#[test]
fn uniform_lifetime_varies_connection_lengths() {
    // persistent_ratio=1.0, lifetime=Uniform(5,15)
    // Connections should close at varying intervals between 5 and 15.
    let transformer = ConnectionPolicyTransformer::new(
        Box::new(NoopTransformer),
        ConnectionPolicy::Mixed {
            persistent_ratio: 1.0,
            connection_lifetime: Some(CountDistribution::Uniform { min: 5, max: 15 }),
        },
    );

    let ctx = make_context();
    let mut close_indices = Vec::new();
    for i in 0..200 {
        let spec = transformer.transform(make_spec(), &ctx);
        if has_connection_close(&spec) {
            close_indices.push(i);
        }
    }

    // Should have multiple close events
    assert!(
        close_indices.len() >= 10,
        "uniform(5,15) over 200 requests should produce >=10 closes, got {}",
        close_indices.len()
    );

    // Check that intervals between closes vary (not all the same)
    let intervals: Vec<usize> = close_indices.windows(2).map(|w| w[1] - w[0]).collect();
    let all_same = intervals.windows(2).all(|w| w[0] == w[1]);
    assert!(
        !all_same,
        "uniform distribution should produce varying intervals, got {:?}",
        intervals
    );

    // All intervals should be in [5, 15]
    for &interval in &intervals {
        assert!(
            (5..=15).contains(&interval),
            "uniform(5,15) produced interval {interval}, expected 5..=15. All: {:?}",
            intervals
        );
    }
}

#[test]
fn normal_lifetime_clusters_around_mean() {
    // persistent_ratio=1.0, lifetime=Normal(50, 5)
    // Connections should close around 50 requests with stddev 5.
    let transformer = ConnectionPolicyTransformer::new(
        Box::new(NoopTransformer),
        ConnectionPolicy::Mixed {
            persistent_ratio: 1.0,
            connection_lifetime: Some(CountDistribution::Normal {
                mean: 50.0,
                stddev: 5.0,
            }),
        },
    );

    let ctx = make_context();
    let mut close_indices = Vec::new();
    for i in 0..500 {
        let spec = transformer.transform(make_spec(), &ctx);
        if has_connection_close(&spec) {
            close_indices.push(i);
        }
    }

    // Should have ~10 closes (500 / 50)
    assert!(
        close_indices.len() >= 5 && close_indices.len() <= 20,
        "normal(50,5) over 500 requests should produce ~10 closes, got {}",
        close_indices.len()
    );

    // Check intervals are roughly centered around 50
    let intervals: Vec<usize> = std::iter::once(close_indices[0])
        .chain(close_indices.windows(2).map(|w| w[1] - w[0]))
        .collect();
    let avg: f64 = intervals.iter().sum::<usize>() as f64 / intervals.len() as f64;
    assert!(
        (35.0..=65.0).contains(&avg),
        "normal(50,5) average interval should be ~50, got {avg:.1}. Intervals: {:?}",
        intervals
    );
}

#[test]
fn connection_policy_preserves_inner_transformer_headers() {
    use netanvil_core::HeaderTransformer;

    let inner = Box::new(HeaderTransformer::new(vec![(
        "X-Custom".into(),
        "value".into(),
    )]));

    let transformer = ConnectionPolicyTransformer::new(inner, ConnectionPolicy::AlwaysNew);

    let ctx = make_context();
    let spec = transformer.transform(make_spec(), &ctx);

    let has_custom = spec.headers.iter().any(|(k, _)| k == "X-Custom");
    assert!(has_custom, "inner transformer headers should be preserved");
    assert!(
        has_connection_close(&spec),
        "AlwaysNew should add Connection: close"
    );
}

#[test]
fn connection_policy_forwards_update_headers_to_inner() {
    use netanvil_core::HeaderTransformer;

    let inner = Box::new(HeaderTransformer::new(vec![(
        "X-Original".into(),
        "yes".into(),
    )]));

    let transformer = ConnectionPolicyTransformer::new(inner, ConnectionPolicy::AlwaysNew);

    transformer.update_headers(vec![("X-New".into(), "updated".into())]);

    let ctx = make_context();
    let spec = transformer.transform(make_spec(), &ctx);

    let has_new = spec.headers.iter().any(|(k, _)| k == "X-New");
    let has_original = spec.headers.iter().any(|(k, _)| k == "X-Original");
    assert!(has_new, "updated header should be present");
    assert!(!has_original, "original header should be replaced");
    assert!(
        has_connection_close(&spec),
        "Connection: close should still be added"
    );
}
