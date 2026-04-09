//! Benchmarks for on_response callback overhead across plugin runtimes.
//!
//! Measures three cost dimensions:
//! 1. Per-call on_response overhead (the callback itself)
//! 2. wants_responses() detection cost (one-time at init)
//! 3. Combined generate() + on_response() throughput vs generate()-only
//!
//! Each runtime is tested with two response profiles:
//! - Minimal: status check only (no headers/body — fastest path)
//! - With headers: response_config = { headers = true, body = false }
//!
//! Run: `cargo bench -p netanvil-plugin -- response`

use std::time::{Duration, Instant};

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use netanvil_plugin::wasm::{compile_wasm_module, WasmGenerator};
use netanvil_plugin_luajit::LuaJitGenerator;
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
        sent_time: now,
        dispatch_time: now,
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
        sent_time: now,
        actual_time: now,
        dispatch_time: now,
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
// on_response() per-call overhead
// ---------------------------------------------------------------------------

fn bench_on_response_percall(c: &mut Criterion) {
    let mut group = c.benchmark_group("on_response_percall");

    // LuaJIT — minimal (no headers)
    let script_minimal = include_str!("../examples/scripts/response_handler_minimal.lua");
    let mut gen_minimal: LuaJitGenerator<HttpRequestSpec> =
        LuaJitGenerator::new(script_minimal, &targets()).expect("LuaJIT init");

    assert!(
        gen_minimal.wants_responses(),
        "minimal script should want responses"
    );

    group.bench_function("luajit_minimal", |b| {
        let mut id = 0u64;
        b.iter(|| {
            id += 1;
            let result = make_result(id, false);
            black_box(gen_minimal.on_response(&result));
        })
    });

    // LuaJIT — with headers
    let script_headers = include_str!("../examples/scripts/response_handler.lua");
    let mut gen_headers: LuaJitGenerator<HttpRequestSpec> =
        LuaJitGenerator::new(script_headers, &targets()).expect("LuaJIT init");

    group.bench_function("luajit_with_headers", |b| {
        let mut id = 0u64;
        b.iter(|| {
            id += 1;
            let result = make_result(id, true);
            black_box(gen_headers.on_response(&result));
        })
    });

    // WASM — with on_response + response_config (headers only)
    let wasm_bytes = include_bytes!("../testdata/netanvil_guest_wasm_response.wasm");
    let (engine, module) = compile_wasm_module(wasm_bytes).expect("WASM compile");
    let mut wasm_gen: WasmGenerator<HttpRequestSpec> =
        WasmGenerator::new(&engine, &module, &targets()).expect("WASM init");

    assert!(
        wasm_gen.wants_responses(),
        "WASM guest should export on_response"
    );

    group.bench_function("wasm_with_headers", |b| {
        let mut id = 0u64;
        b.iter(|| {
            id += 1;
            let result = make_result(id, true);
            black_box(wasm_gen.on_response(&result));
        })
    });

    // WASM — minimal response (no headers in result, but flags say yes)
    // This benchmarks the RawResult-only fast path when there's no var data.
    group.bench_function("wasm_no_var_data", |b| {
        let mut id = 0u64;
        b.iter(|| {
            id += 1;
            let result = make_result(id, false); // no headers in result
            black_box(wasm_gen.on_response(&result));
        })
    });

    // Rhai — minimal
    let rhai_script = r#"
        let ok_count = 0;
        fn response_config() { #{ headers: false, body: false } }
        fn on_response(result) { if result.status == 200 { ok_count += 1; } }
        fn generate(ctx) { #{ method: "GET", url: "http://localhost" } }
    "#;
    let mut rhai_gen: netanvil_plugin::RhaiGenerator<HttpRequestSpec> =
        netanvil_plugin::RhaiGenerator::new(rhai_script, &targets()).expect("Rhai init");

    assert!(rhai_gen.wants_responses());

    group.bench_function("rhai_minimal", |b| {
        let mut id = 0u64;
        b.iter(|| {
            id += 1;
            let result = make_result(id, false);
            black_box(rhai_gen.on_response(&result));
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Generate-only vs Generate+on_response throughput comparison
// ---------------------------------------------------------------------------

fn bench_generate_vs_generate_with_response(c: &mut Criterion) {
    let mut group = c.benchmark_group("generate_with_response_1000");
    group.sample_size(50);

    // LuaJIT — generate only (no on_response defined)
    let gen_script = include_str!("../examples/scripts/generator.lua");
    let mut gen_only: LuaJitGenerator<HttpRequestSpec> =
        LuaJitGenerator::new(gen_script, &targets()).expect("LuaJIT init");

    assert!(!gen_only.wants_responses());

    group.bench_function("luajit_generate_only_1000", |b| {
        b.iter(|| {
            for i in 0..1000u64 {
                let ctx = make_context(i);
                black_box(gen_only.generate(&ctx));
            }
        })
    });

    // LuaJIT — generate + on_response (minimal)
    let resp_script = include_str!("../examples/scripts/response_handler_minimal.lua");
    let mut gen_resp: LuaJitGenerator<HttpRequestSpec> =
        LuaJitGenerator::new(resp_script, &targets()).expect("LuaJIT init");

    group.bench_function("luajit_generate_plus_on_response_1000", |b| {
        b.iter(|| {
            for i in 0..1000u64 {
                let ctx = make_context(i);
                black_box(gen_resp.generate(&ctx));
                let result = make_result(i, false);
                black_box(gen_resp.on_response(&result));
            }
        })
    });

    // LuaJIT — generate + on_response (with headers)
    let resp_hdr_script = include_str!("../examples/scripts/response_handler.lua");
    let mut gen_resp_hdr: LuaJitGenerator<HttpRequestSpec> =
        LuaJitGenerator::new(resp_hdr_script, &targets()).expect("LuaJIT init");

    group.bench_function("luajit_generate_plus_on_response_headers_1000", |b| {
        b.iter(|| {
            for i in 0..1000u64 {
                let ctx = make_context(i);
                black_box(gen_resp_hdr.generate(&ctx));
                let result = make_result(i, true);
                black_box(gen_resp_hdr.on_response(&result));
            }
        })
    });

    // WASM — generate only (no on_response)
    let wasm_bytes_plain = include_bytes!("../testdata/netanvil_guest_wasm.wasm");
    let (engine_plain, module_plain) = compile_wasm_module(wasm_bytes_plain).unwrap();
    let mut wasm_gen_only: WasmGenerator<HttpRequestSpec> =
        WasmGenerator::new(&engine_plain, &module_plain, &targets()).unwrap();

    assert!(!wasm_gen_only.wants_responses());

    group.bench_function("wasm_generate_only_1000", |b| {
        b.iter(|| {
            for i in 0..1000u64 {
                let ctx = make_context(i);
                black_box(wasm_gen_only.generate(&ctx));
            }
        })
    });

    // WASM — generate + on_response (with headers)
    let wasm_bytes_resp = include_bytes!("../testdata/netanvil_guest_wasm_response.wasm");
    let (engine_resp, module_resp) = compile_wasm_module(wasm_bytes_resp).unwrap();
    let mut wasm_gen_resp: WasmGenerator<HttpRequestSpec> =
        WasmGenerator::new(&engine_resp, &module_resp, &targets()).unwrap();

    group.bench_function("wasm_generate_plus_on_response_headers_1000", |b| {
        b.iter(|| {
            for i in 0..1000u64 {
                let ctx = make_context(i);
                black_box(wasm_gen_resp.generate(&ctx));
                let result = make_result(i, true);
                black_box(wasm_gen_resp.on_response(&result));
            }
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Instance creation overhead with response callback detection
// ---------------------------------------------------------------------------

fn bench_instance_creation_response(c: &mut Criterion) {
    let mut group = c.benchmark_group("instance_creation_response");

    let tgts = targets();

    // LuaJIT — script WITHOUT on_response (baseline)
    let gen_script = include_str!("../examples/scripts/generator.lua");
    group.bench_function("luajit_no_on_response", |b| {
        b.iter(|| {
            black_box(LuaJitGenerator::<HttpRequestSpec>::new(gen_script, &tgts).unwrap());
        })
    });

    // LuaJIT — script WITH on_response + response_config
    let resp_script = include_str!("../examples/scripts/response_handler.lua");
    group.bench_function("luajit_with_on_response", |b| {
        b.iter(|| {
            black_box(LuaJitGenerator::<HttpRequestSpec>::new(resp_script, &tgts).unwrap());
        })
    });

    // WASM — module WITHOUT on_response (baseline)
    let wasm_plain = include_bytes!("../testdata/netanvil_guest_wasm.wasm");
    let (eng_plain, mod_plain) = compile_wasm_module(wasm_plain).unwrap();
    group.bench_function("wasm_no_on_response", |b| {
        b.iter(|| {
            black_box(
                WasmGenerator::<HttpRequestSpec>::new(&eng_plain, &mod_plain, &tgts).unwrap(),
            );
        })
    });

    // WASM — module WITH on_response + response_config
    let wasm_resp = include_bytes!("../testdata/netanvil_guest_wasm_response.wasm");
    let (eng_resp, mod_resp) = compile_wasm_module(wasm_resp).unwrap();
    group.bench_function("wasm_with_on_response", |b| {
        b.iter(|| {
            black_box(WasmGenerator::<HttpRequestSpec>::new(&eng_resp, &mod_resp, &tgts).unwrap());
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// wants_responses() cost (should be effectively zero — cached bool)
// ---------------------------------------------------------------------------

fn bench_wants_responses(c: &mut Criterion) {
    let script = include_str!("../examples/scripts/response_handler.lua");
    let gen: LuaJitGenerator<HttpRequestSpec> =
        LuaJitGenerator::new(script, &targets()).expect("LuaJIT init");

    c.bench_function("wants_responses_check", |b| {
        b.iter(|| {
            black_box(gen.wants_responses());
        })
    });
}

criterion_group!(
    benches,
    bench_on_response_percall,
    bench_generate_vs_generate_with_response,
    bench_instance_creation_response,
    bench_wants_responses,
);
criterion_main!(benches);
