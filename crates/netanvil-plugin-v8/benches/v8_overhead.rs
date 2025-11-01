//! Benchmarks for the V8 JavaScript plugin runtime.
//!
//! Measures per-call generate() overhead, on_response() callback cost,
//! instance creation, and sustained throughput — matching the structure
//! of the main plugin benchmark suite in netanvil-plugin.
//!
//! Run: `cargo bench -p netanvil-plugin-v8`

use std::time::{Duration, Instant};

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use netanvil_plugin_v8::V8Generator;
use netanvil_types::{
    ExecutionResult, HttpRequestSpec, RequestContext, RequestGenerator, TimingBreakdown,
};

const TARGETS: &[&str] = &[
    "http://api.example.com/users",
    "http://api.example.com/items",
    "http://api.example.com/orders",
];

fn make_context(request_id: u64) -> RequestContext {
    let now = Instant::now();
    RequestContext {
        request_id,
        intended_time: now,
        actual_time: now,
        core_id: 0,
        is_sampled: false,
        session_id: None,
    }
}

fn make_result(request_id: u64, with_headers: bool) -> ExecutionResult {
    let now = Instant::now();
    ExecutionResult {
        request_id,
        intended_time: now,
        actual_time: now,
        timing: TimingBreakdown {
            total: Duration::from_micros(500),
            time_to_first_byte: Duration::from_micros(400),
            ..Default::default()
        },
        status: Some(200),
        bytes_sent: 128,
        response_size: 4096,
        error: None,
        response_headers: if with_headers {
            Some(vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                (
                    "X-Session-Token".to_string(),
                    "tok-abc123def456".to_string(),
                ),
                ("X-Request-ID".to_string(), format!("{request_id}")),
                ("X-Cache".to_string(), "HIT".to_string()),
            ])
        } else {
            None
        },
        response_body: None,
    }
}

fn targets() -> Vec<String> {
    TARGETS.iter().map(|s| s.to_string()).collect()
}

// ---------------------------------------------------------------------------
// Per-call generate() overhead
// ---------------------------------------------------------------------------

fn bench_v8_generate(c: &mut Criterion) {
    let script = include_str!("../../netanvil-plugin/examples/scripts/generator.js");
    let mut gen: V8Generator<HttpRequestSpec> =
        V8Generator::new(script, &targets()).expect("V8 init failed");
    let mut id = 0u64;

    c.bench_function("v8_generate", |b| {
        b.iter(|| {
            id += 1;
            let ctx = make_context(id);
            black_box(gen.generate(&ctx));
        })
    });
}

// ---------------------------------------------------------------------------
// on_response() per-call overhead
// ---------------------------------------------------------------------------

fn bench_on_response_percall(c: &mut Criterion) {
    let mut group = c.benchmark_group("v8_on_response_percall");

    // Minimal (no headers)
    let script_minimal =
        include_str!("../../netanvil-plugin/examples/scripts/response_handler_minimal.js");
    let mut gen_minimal: V8Generator<HttpRequestSpec> =
        V8Generator::new(script_minimal, &targets()).expect("V8 init");
    assert!(gen_minimal.wants_responses());

    group.bench_function("minimal", |b| {
        let mut id = 0u64;
        b.iter(|| {
            id += 1;
            let result = make_result(id, false);
            black_box(gen_minimal.on_response(&result));
        })
    });

    // With headers
    let script_headers =
        include_str!("../../netanvil-plugin/examples/scripts/response_handler.js");
    let mut gen_headers: V8Generator<HttpRequestSpec> =
        V8Generator::new(script_headers, &targets()).expect("V8 init");

    group.bench_function("with_headers", |b| {
        let mut id = 0u64;
        b.iter(|| {
            id += 1;
            let result = make_result(id, true);
            black_box(gen_headers.on_response(&result));
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Instance creation (one-time cost)
// ---------------------------------------------------------------------------

fn bench_instance_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("v8_instance_creation");
    let tgts = targets();

    // Without on_response
    let gen_script = include_str!("../../netanvil-plugin/examples/scripts/generator.js");
    group.bench_function("no_on_response", |b| {
        b.iter(|| {
            black_box(V8Generator::<HttpRequestSpec>::new(gen_script, &tgts).unwrap());
        })
    });

    // With on_response + response_config
    let resp_script =
        include_str!("../../netanvil-plugin/examples/scripts/response_handler.js");
    group.bench_function("with_on_response", |b| {
        b.iter(|| {
            black_box(V8Generator::<HttpRequestSpec>::new(resp_script, &tgts).unwrap());
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Throughput (sustained 1000-call batches)
// ---------------------------------------------------------------------------

fn bench_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("v8_throughput_1000_calls");
    group.sample_size(50);

    // Generate only
    let gen_script = include_str!("../../netanvil-plugin/examples/scripts/generator.js");
    let mut gen: V8Generator<HttpRequestSpec> =
        V8Generator::new(gen_script, &targets()).unwrap();

    group.bench_function("generate_only", |b| {
        b.iter(|| {
            for i in 0..1000u64 {
                let ctx = make_context(i);
                black_box(gen.generate(&ctx));
            }
        })
    });

    // Generate + on_response (minimal)
    let resp_script =
        include_str!("../../netanvil-plugin/examples/scripts/response_handler_minimal.js");
    let mut gen_resp: V8Generator<HttpRequestSpec> =
        V8Generator::new(resp_script, &targets()).unwrap();

    group.bench_function("generate_plus_on_response", |b| {
        b.iter(|| {
            for i in 0..1000u64 {
                let ctx = make_context(i);
                black_box(gen_resp.generate(&ctx));
                let result = make_result(i, false);
                black_box(gen_resp.on_response(&result));
            }
        })
    });

    // Generate + on_response (with headers)
    let resp_hdr_script =
        include_str!("../../netanvil-plugin/examples/scripts/response_handler.js");
    let mut gen_resp_hdr: V8Generator<HttpRequestSpec> =
        V8Generator::new(resp_hdr_script, &targets()).unwrap();

    group.bench_function("generate_plus_on_response_headers", |b| {
        b.iter(|| {
            for i in 0..1000u64 {
                let ctx = make_context(i);
                black_box(gen_resp_hdr.generate(&ctx));
                let result = make_result(i, true);
                black_box(gen_resp_hdr.on_response(&result));
            }
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// wants_responses() cost (should be effectively zero — cached bool)
// ---------------------------------------------------------------------------

fn bench_wants_responses(c: &mut Criterion) {
    let script = include_str!("../../netanvil-plugin/examples/scripts/response_handler.js");
    let gen: V8Generator<HttpRequestSpec> =
        V8Generator::new(script, &targets()).expect("V8 init");

    c.bench_function("v8_wants_responses_check", |b| {
        b.iter(|| {
            black_box(gen.wants_responses());
        })
    });
}

criterion_group!(
    benches,
    bench_v8_generate,
    bench_on_response_percall,
    bench_instance_creation,
    bench_throughput,
    bench_wants_responses,
);
criterion_main!(benches);
