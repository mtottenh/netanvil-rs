//! Benchmarks comparing per-call overhead of each plugin runtime.
//!
//! Measures the cost of calling `generate()` with struct marshaling for:
//! - Native Rust (baseline)
//! - WASM via wasmtime (JSON marshaling through linear memory)
//! - LuaJIT via netanvil-plugin-luajit
//! - Rhai scripting engine
//! - Hybrid (plugin config → native execution)
//!
//! Run: `cargo bench -p netanvil-plugin`
//! For Lua 5.4 benchmarks: `cargo bench -p netanvil-plugin-lua54`

use std::time::Instant;

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use netanvil_plugin::hybrid::{GeneratorConfig, HybridGenerator, WeightedPattern};
use netanvil_plugin::rhai_runtime::RhaiGenerator;
use netanvil_plugin::wasm::{compile_wasm_module, WasmGenerator};
use netanvil_plugin_luajit::LuaJitGenerator;
use netanvil_types::{RequestContext, RequestGenerator};

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

fn targets() -> Vec<String> {
    TARGETS.iter().map(|s| s.to_string()).collect()
}

// ---------------------------------------------------------------------------
// Native Rust baseline
// ---------------------------------------------------------------------------

struct NativeGenerator {
    targets: Vec<String>,
    counter: u64,
}

impl NativeGenerator {
    fn new(targets: Vec<String>) -> Self {
        Self {
            targets,
            counter: 0,
        }
    }
}

impl RequestGenerator for NativeGenerator {
    type Spec = netanvil_types::HttpRequestSpec;

    fn generate(&mut self, ctx: &RequestContext) -> netanvil_types::HttpRequestSpec {
        let idx = (self.counter as usize) % self.targets.len();
        let url = format!(
            "{}?seq={}&core={}",
            self.targets[idx], self.counter, ctx.core_id
        );
        let body = format!(
            r#"{{"request_id":{},"seq":{},"core_id":{}}}"#,
            ctx.request_id, self.counter, ctx.core_id
        );
        self.counter += 1;
        netanvil_types::HttpRequestSpec {
            method: http::Method::GET,
            url,
            headers: vec![
                ("Content-Type".into(), "application/json".into()),
                ("X-Request-ID".into(), format!("{}", ctx.request_id)),
            ],
            body: Some(body.into_bytes()),
        }
    }
}

// ---------------------------------------------------------------------------
// Benchmark functions
// ---------------------------------------------------------------------------

fn bench_native(c: &mut Criterion) {
    let mut gen = NativeGenerator::new(targets());
    let mut id = 0u64;

    c.bench_function("native_rust_generate", |b| {
        b.iter(|| {
            id += 1;
            let ctx = make_context(id);
            black_box(gen.generate(&ctx));
        })
    });
}

fn bench_wasm(c: &mut Criterion) {
    let wasm_bytes = include_bytes!("../testdata/netanvil_guest_wasm.wasm");

    let (engine, module) = compile_wasm_module(wasm_bytes).expect("WASM compile failed");
    let mut gen = WasmGenerator::new(&engine, &module, &targets()).expect("WASM init failed");
    let mut id = 0u64;

    c.bench_function("wasm_generate_json", |b| {
        b.iter(|| {
            id += 1;
            let ctx = make_context(id);
            black_box(gen.generate(&ctx));
        })
    });
}

fn bench_luajit(c: &mut Criterion) {
    let script = include_str!("../examples/scripts/generator.lua");
    let mut gen = LuaJitGenerator::new(script, &targets()).expect("LuaJIT init failed");
    let mut id = 0u64;

    c.bench_function("luajit_generate", |b| {
        b.iter(|| {
            id += 1;
            let ctx = make_context(id);
            black_box(gen.generate(&ctx));
        })
    });
}

fn bench_rhai(c: &mut Criterion) {
    let script = include_str!("../examples/scripts/generator.rhai");
    let mut gen = RhaiGenerator::new(script, &targets()).expect("Rhai init failed");
    let mut id = 0u64;

    c.bench_function("rhai_generate", |b| {
        b.iter(|| {
            id += 1;
            let ctx = make_context(id);
            black_box(gen.generate(&ctx));
        })
    });
}

fn bench_hybrid(c: &mut Criterion) {
    let config = GeneratorConfig {
        url_patterns: vec![
            WeightedPattern {
                pattern: "http://api.example.com/users?seq={seq}&core={core_id}".into(),
                weight: 1.0,
            },
            WeightedPattern {
                pattern: "http://api.example.com/items?seq={seq}&core={core_id}".into(),
                weight: 1.0,
            },
            WeightedPattern {
                pattern: "http://api.example.com/orders?seq={seq}&core={core_id}".into(),
                weight: 1.0,
            },
        ],
        method: "GET".into(),
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            ("X-Request-ID".into(), "{request_id}".into()),
        ],
        body_template: Some(
            r#"{"request_id":{request_id},"seq":{seq},"core_id":{core_id}}"#.into(),
        ),
    };

    let mut gen = HybridGenerator::new(config);
    let mut id = 0u64;

    c.bench_function("hybrid_generate", |b| {
        b.iter(|| {
            id += 1;
            let ctx = make_context(id);
            black_box(gen.generate(&ctx));
        })
    });
}

// ---------------------------------------------------------------------------
// Instance creation benchmarks (one-time cost)
// ---------------------------------------------------------------------------

fn bench_instance_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("instance_creation");

    let wasm_bytes = include_bytes!("../testdata/netanvil_guest_wasm.wasm");
    let (engine, module) = compile_wasm_module(wasm_bytes).expect("WASM compile failed");
    let tgts = targets();

    group.bench_function("wasm_instance", |b| {
        b.iter(|| {
            black_box(WasmGenerator::new(&engine, &module, &tgts).unwrap());
        })
    });

    let lua_script = include_str!("../examples/scripts/generator.lua");
    group.bench_function("luajit_instance", |b| {
        b.iter(|| {
            black_box(LuaJitGenerator::new(lua_script, &tgts).unwrap());
        })
    });

    let rhai_script = include_str!("../examples/scripts/generator.rhai");
    group.bench_function("rhai_instance", |b| {
        b.iter(|| {
            black_box(RhaiGenerator::new(rhai_script, &tgts).unwrap());
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Throughput benchmarks (sustained calls to measure amortized overhead)
// ---------------------------------------------------------------------------

fn bench_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("throughput_1000_calls");
    group.sample_size(50);

    let mut native_gen = NativeGenerator::new(targets());
    group.bench_function("native_1000", |b| {
        b.iter(|| {
            for i in 0..1000u64 {
                let ctx = make_context(i);
                black_box(native_gen.generate(&ctx));
            }
        })
    });

    let wasm_bytes = include_bytes!("../testdata/netanvil_guest_wasm.wasm");
    let (engine, module) = compile_wasm_module(wasm_bytes).unwrap();
    let mut wasm_gen = WasmGenerator::new(&engine, &module, &targets()).unwrap();
    group.bench_function("wasm_1000", |b| {
        b.iter(|| {
            for i in 0..1000u64 {
                let ctx = make_context(i);
                black_box(wasm_gen.generate(&ctx));
            }
        })
    });

    let lua_script = include_str!("../examples/scripts/generator.lua");
    let mut lua_gen = LuaJitGenerator::new(lua_script, &targets()).unwrap();
    group.bench_function("luajit_1000", |b| {
        b.iter(|| {
            for i in 0..1000u64 {
                let ctx = make_context(i);
                black_box(lua_gen.generate(&ctx));
            }
        })
    });

    let rhai_script = include_str!("../examples/scripts/generator.rhai");
    let mut rhai_gen = RhaiGenerator::new(rhai_script, &targets()).unwrap();
    group.bench_function("rhai_1000", |b| {
        b.iter(|| {
            for i in 0..1000u64 {
                let ctx = make_context(i);
                black_box(rhai_gen.generate(&ctx));
            }
        })
    });

    let config = GeneratorConfig {
        url_patterns: TARGETS
            .iter()
            .map(|t| WeightedPattern {
                pattern: format!("{t}?seq={{seq}}&core={{core_id}}"),
                weight: 1.0,
            })
            .collect(),
        method: "GET".into(),
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            ("X-Request-ID".into(), "{request_id}".into()),
        ],
        body_template: Some(
            r#"{"request_id":{request_id},"seq":{seq},"core_id":{core_id}}"#.into(),
        ),
    };
    let mut hybrid_gen = HybridGenerator::new(config);
    group.bench_function("hybrid_1000", |b| {
        b.iter(|| {
            for i in 0..1000u64 {
                let ctx = make_context(i);
                black_box(hybrid_gen.generate(&ctx));
            }
        })
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_native,
    bench_wasm,
    bench_luajit,
    bench_rhai,
    bench_hybrid,
    bench_instance_creation,
    bench_throughput,
);
criterion_main!(benches);
