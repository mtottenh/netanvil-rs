# Plugin System

netanvil-rs supports four plugin types for custom request generation, ranging
from zero-overhead declarative configuration to full per-request programmability.

## Quick Start

```bash
# Hybrid: Lua configures patterns, native Rust executes (fastest)
netanvil-cli test --url http://api.example.com --plugin config.lua --plugin-type hybrid

# Lua: per-request scripting via LuaJIT
netanvil-cli test --url http://api.example.com --plugin generator.lua

# WASM: compiled module (Rust, C, Go, etc.)
netanvil-cli test --url http://api.example.com --plugin generator.wasm
```

The `--plugin-type` flag accepts `hybrid`, `lua`, or `wasm`. If omitted, it is
auto-detected from the file extension (`.lua` defaults to per-request Lua,
`.wasm` to WASM).

## Plugin Types at a Glance

| Capability                        | Hybrid       | Lua (LuaJIT)  | WASM (wasmtime) |
|-----------------------------------|--------------|----------------|-----------------|
| Per-call overhead                 | ~576ns       | ~2.1us         | ~878ns          |
| Max RPS/core                      | 50K+         | ~40K           | ~100K+          |
| Conditional logic per request     | No           | Yes            | Yes             |
| Stateful across requests          | Counter only | Yes (globals)  | Yes (globals)   |
| Dynamic HTTP method per request   | No           | Yes            | Yes             |
| Dynamic headers per request       | No           | Yes            | Yes             |
| Session simulation                | No           | Yes            | Yes             |
| External data (files, tables)     | No           | Yes            | Yes             |
| Language                          | Lua (config) | Lua 5.1        | Any -> WASM     |
| Sandboxing                        | N/A          | None           | Full (WASI)     |
| Debugging                         | Easy         | print() + logs | Limited         |

## Hybrid Plugin

The hybrid plugin is the **fastest** option. A Lua script runs **once** at setup
to produce a declarative `GeneratorConfig`. The hot path then executes in pure
native Rust — no scripting engine is invoked per request.

### What It Can Do

- Define weighted URL patterns with placeholders (`{seq}`, `{core_id}`, `{request_id}`)
- Set a fixed HTTP method for all requests
- Set static headers applied to every request
- Define a body template with the same placeholders

### What It Cannot Do

- **No conditional logic**: every request follows the same pattern. You cannot
  say "if request_id is even, use endpoint A; otherwise use endpoint B."
- **No dynamic headers**: headers are fixed at setup. You cannot rotate
  authentication tokens or vary `Content-Type` per request.
- **No dynamic method**: all requests use the same HTTP method.
- **No stateful sequences**: you cannot implement login-then-browse flows or
  build request bodies that depend on the previous request's data.
- **No external data**: you cannot read from CSV files, user pools, or databases.
- **Limited placeholders**: only `{seq}`, `{core_id}`, and `{request_id}` are
  available. No randomness, no timestamps, no computed values.

### Example: Weighted API Endpoints

```lua
-- hybrid_config.lua
-- Called ONCE at startup. Returns a static configuration.
function configure()
    return {
        method = "POST",
        url_patterns = {
            { pattern = "http://api.example.com/users/{request_id}", weight = 3.0 },
            { pattern = "http://api.example.com/items/{seq}",        weight = 1.0 },
        },
        headers = {
            {"Content-Type", "application/json"},
            {"Authorization", "Bearer static-test-token"},
        },
        body_template = '{"id":{request_id},"seq":{seq},"core":{core_id}}',
    }
end
```

```bash
netanvil-cli test --url http://api.example.com \
  --plugin hybrid_config.lua --plugin-type hybrid \
  --rps 50000 --duration 60s --cores 4
```

This generates requests like:
```
POST /users/42?          {"id":42,"seq":0,"core":0}
POST /users/43?          {"id":43,"seq":1,"core":0}
POST /users/44?          {"id":44,"seq":2,"core":0}
POST /items/3?           {"id":45,"seq":3,"core":0}    <- weight 1:3 ratio
```

75% of requests hit `/users`, 25% hit `/items`. All use POST with the same
headers. The body varies only through placeholder expansion.

### Where Hybrid Falls Short

Suppose you need to:
1. Rotate through 10,000 user IDs from a pool
2. Use GET for reads and POST for writes (80/20 split)
3. Include a per-request HMAC signature header

Hybrid cannot do any of these. You need Lua or WASM.

---

## Lua Plugin (LuaJIT)

The Lua plugin runs a `generate(ctx)` function **on every request**. It has
full access to Lua's standard library, can maintain arbitrary state, and can
implement complex request generation logic.

### What It Can Do (Beyond Hybrid)

- **Conditional logic**: vary method, URL, headers, and body based on request
  context, internal state, or randomness
- **Stateful sequences**: implement multi-step flows (login, browse, checkout)
- **Dynamic headers**: rotate auth tokens, compute signatures, add timestamps
- **External data at init**: load user pools, URL lists, or test data from
  tables defined in the script
- **String manipulation**: format, concatenate, pattern-match on URLs and bodies
- **Math/randomness**: Lua's `math.random()` for probabilistic workloads

### Limitations

- **No filesystem/network access at runtime** (by convention — Lua _can_ do
  this but it would block the hot path and destroy performance)
- **No true parallelism**: each core gets its own Lua VM; they don't share state
- **~2.1us overhead per call**: 10x slower than hybrid, but still well within
  the 20us budget at 50K RPS

### Example: Session Simulation with User Pool

This example demonstrates capabilities that are **impossible** with the hybrid
plugin: rotating user IDs, conditional HTTP methods, dynamic auth headers, and
multi-step session flows.

```lua
-- session_generator.lua
-- Simulates realistic user sessions: login -> browse -> purchase

local targets = {}
local counter = 0

-- State that persists across calls (per-core, not shared between cores)
local user_pool = {}
local session_step = {}  -- tracks where each "session" is in its flow

function init(target_list)
    targets = target_list
    counter = 0

    -- Build a pool of 10,000 test users
    for i = 1, 10000 do
        user_pool[i] = {
            id = i,
            name = "user-" .. i,
            token = "tok-" .. string.format("%08x", i * 31337),
        }
    end
end

function generate(ctx)
    counter = counter + 1

    -- Rotate through users (each core has its own pool index)
    local user_idx = ((counter - 1) % #user_pool) + 1
    local user = user_pool[user_idx]

    -- 3-step session: login (10%) -> browse (70%) -> purchase (20%)
    -- This is IMPOSSIBLE in hybrid mode: it requires conditional logic.
    local roll = counter % 10
    local method, path, body

    if roll == 0 then
        -- Step 1: Login (POST)
        method = "POST"
        path = "/api/v1/auth/login"
        body = string.format(
            '{"username":"%s","password":"test-pass-%d"}',
            user.name, user.id
        )
    elseif roll <= 7 then
        -- Step 2: Browse products (GET) -- no body
        -- Dynamic path based on state: IMPOSSIBLE in hybrid.
        local product_id = (counter * 7 + user.id) % 50000
        method = "GET"
        path = "/api/v1/products/" .. product_id
        body = nil
    else
        -- Step 3: Purchase (POST)
        local product_id = (counter * 13 + user.id) % 50000
        method = "POST"
        path = "/api/v1/orders"
        body = string.format(
            '{"user_id":%d,"product_id":%d,"quantity":%d}',
            user.id, product_id, (counter % 5) + 1
        )
    end

    local base_url = targets[((counter - 1) % #targets) + 1]

    return {
        method = method,
        url = base_url .. path,
        headers = {
            {"Content-Type", "application/json"},
            -- Dynamic per-user auth header: IMPOSSIBLE in hybrid.
            {"Authorization", "Bearer " .. user.token},
            -- Per-request correlation ID
            {"X-Request-ID", tostring(ctx.request_id)},
            -- Custom header with computed value
            {"X-Session-Step", tostring(roll)},
        },
        body = body
    }
end
```

```bash
netanvil-cli test --url http://api.example.com \
  --plugin session_generator.lua \
  --rps 40000 --duration 120s --cores 4
```

This generates a realistic mixed workload:
```
POST /api/v1/auth/login        Authorization: Bearer tok-00007a69  {"username":"user-1",...}
GET  /api/v1/products/3847     Authorization: Bearer tok-0000f4d2  (no body)
GET  /api/v1/products/9182     Authorization: Bearer tok-00016e3b  (no body)
POST /api/v1/orders            Authorization: Bearer tok-0001e7a4  {"user_id":4,...}
GET  /api/v1/products/22491    Authorization: Bearer tok-000260d9  (no body)
...
```

**Key things hybrid cannot do here:**
- Mixed HTTP methods (GET/POST) based on logic
- Per-user `Authorization` header rotating through 10,000 tokens
- Conditional URL paths (`/auth/login` vs `/products/N` vs `/orders`)
- Computed body content that depends on internal state
- Session step tracking

---

## WASM Plugin

The WASM plugin loads a pre-compiled WebAssembly module and calls its
`generate()` function on every request. The module can be written in **any
language** that compiles to WASM (Rust, C, C++, Go, AssemblyScript, Zig).

### What It Can Do (Beyond Lua)

Everything Lua can do, plus:

- **Any language**: write plugins in Rust, C, Go, AssemblyScript — not just Lua
- **Full sandboxing**: WASM runs in an isolated memory space with no host access
  unless explicitly granted. Safe for untrusted plugins.
- **Compiled performance**: complex logic (crypto, compression, parsing) runs
  at near-native speed inside the WASM VM
- **Deterministic execution**: no GC pauses, no JIT warmup surprises
- **Large data structures**: WASM linear memory can hold hash maps, bloom
  filters, or pre-computed lookup tables efficiently
- **Library ecosystem**: use any Rust/C library that compiles to wasm32-wasip1

### Limitations

- **Harder development workflow**: write code, compile to `.wasm`, then load
- **Debugging is harder**: no `print()` equivalent without WASI stdout wiring
- **Binary protocol overhead**: ~878ns per call (postcard encoding + WASM call transitions)
- **Binary size**: ~40KB for a minimal plugin (includes allocator and postcard)

### Example: HMAC-Signed Requests with Bloom Filter Dedup

This example demonstrates capabilities that are **difficult or impossible in
Lua**: cryptographic HMAC signing, a bloom filter for deduplication, and
leveraging Rust's full standard library and crate ecosystem.

```rust
// wasm_generator/src/lib.rs
// Compile: cargo build --target wasm32-wasip1 --release
//
// This generator:
// 1. Produces unique request IDs using a bloom filter (avoids duplicates)
// 2. Signs each request body with HMAC-SHA256
// 3. Implements a state machine for multi-phase load testing
//
// These capabilities are difficult in Lua (no crypto stdlib, no bloom filter)
// and impossible in hybrid mode.

use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// 24-byte context received from the host via shared memory.
/// Must match the host's RawContext layout exactly.
#[repr(C)]
#[derive(Clone, Copy)]
struct RawContext {
    request_id: u64,
    core_id: u32,
    flags: u8,       // bit 0: is_sampled, bit 1: has_session_id
    _pad: [u8; 3],
    session_id: u64,
}
const _: () = assert!(core::mem::size_of::<RawContext>() == 24);

#[derive(Serialize)]
struct HttpRequestSpec {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
}

// --- Bloom filter for request deduplication ---

struct BloomFilter {
    bits: Vec<u64>,
    num_hashes: usize,
}

impl BloomFilter {
    fn new(size_bits: usize, num_hashes: usize) -> Self {
        Self {
            bits: vec![0u64; (size_bits + 63) / 64],
            num_hashes,
        }
    }

    fn insert(&mut self, item: u64) {
        for i in 0..self.num_hashes {
            let hash = self.hash(item, i as u64);
            let idx = hash as usize % (self.bits.len() * 64);
            self.bits[idx / 64] |= 1 << (idx % 64);
        }
    }

    fn might_contain(&self, item: u64) -> bool {
        for i in 0..self.num_hashes {
            let hash = self.hash(item, i as u64);
            let idx = hash as usize % (self.bits.len() * 64);
            if self.bits[idx / 64] & (1 << (idx % 64)) == 0 {
                return false;
            }
        }
        true
    }

    fn hash(&self, item: u64, seed: u64) -> u64 {
        let mut hasher = DefaultHasher::new();
        item.hash(&mut hasher);
        seed.hash(&mut hasher);
        hasher.finish()
    }
}

// --- Simple HMAC-SHA256 (using basic hash, not cryptographic in this example) ---

fn hmac_sign(key: &[u8], message: &[u8]) -> String {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    message.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

// --- Test phases ---

#[derive(Clone, Copy)]
enum Phase {
    Warmup,     // gentle ramp: only GETs
    Steady,     // mixed read/write
    Spike,      // burst of writes to stress the system
}

// --- Global state ---

static mut COUNTER: u64 = 0;
static mut TARGETS: Vec<String> = Vec::new();
static mut OUTPUT_BUF: Vec<u8> = Vec::new();
static mut INITIALIZED: bool = false;
static mut BLOOM: Option<BloomFilter> = None;
static mut PHASE_THRESHOLDS: (u64, u64) = (1000, 5000); // warmup until 1K, spike at 5K
static mut SIGNING_KEY: Vec<u8> = Vec::new();

const INPUT_BUF_SIZE: usize = 65536;
static mut INPUT_BUF: [u8; INPUT_BUF_SIZE] = [0u8; INPUT_BUF_SIZE];

#[no_mangle]
pub extern "C" fn get_input_buf_ptr() -> *mut u8 {
    unsafe { INPUT_BUF.as_mut_ptr() }
}

#[no_mangle]
pub extern "C" fn get_output_buf_ptr() -> *const u8 {
    unsafe { OUTPUT_BUF.as_ptr() }
}

#[no_mangle]
pub extern "C" fn init(input_len: u32) -> u32 {
    unsafe {
        let input = &INPUT_BUF[..input_len as usize];
        match postcard::from_bytes::<Vec<String>>(input) {
            Ok(targets) => {
                TARGETS = targets;
                COUNTER = 0;
                INITIALIZED = true;
                // 1M-bit bloom filter, 3 hash functions
                BLOOM = Some(BloomFilter::new(1_000_000, 3));
                SIGNING_KEY = b"netanvil-rs-test-key-2024".to_vec();
                0
            }
            Err(_) => 1,
        }
    }
}

#[no_mangle]
pub extern "C" fn generate(input_len: u32) -> u32 {
    unsafe {
        if !INITIALIZED || TARGETS.is_empty() {
            return 0;
        }

        // Read 24-byte binary context directly (zero deserialization).
        if (input_len as usize) < core::mem::size_of::<RawContext>() {
            return 0;
        }
        let ctx = &*(INPUT_BUF.as_ptr() as *const RawContext);

        COUNTER += 1;
        let counter = COUNTER;

        // Determine test phase based on request count.
        // This kind of state machine is impossible in hybrid mode.
        let phase = if counter < PHASE_THRESHOLDS.0 {
            Phase::Warmup
        } else if counter >= PHASE_THRESHOLDS.1 {
            Phase::Spike
        } else {
            Phase::Steady
        };

        // Generate a unique entity ID, checking bloom filter for collisions.
        // Lua has no bloom filter; you'd need to maintain a huge table.
        let bloom = BLOOM.as_mut().unwrap();
        let mut entity_id = (counter * 31 + ctx.core_id as u64 * 1_000_000) % 10_000_000;
        while bloom.might_contain(entity_id) {
            entity_id = entity_id.wrapping_add(1);
        }
        bloom.insert(entity_id);

        let (method, path, body_str) = match phase {
            Phase::Warmup => {
                // Warmup: only reads
                ("GET", format!("/api/v1/entities/{entity_id}"), None)
            }
            Phase::Steady => {
                // Steady: 70% reads, 30% writes
                if counter % 10 < 7 {
                    ("GET", format!("/api/v1/entities/{entity_id}"), None)
                } else {
                    let body = format!(
                        r#"{{"entity_id":{entity_id},"value":"data-{counter}","core":{}}}"#,
                        ctx.core_id
                    );
                    ("PUT", format!("/api/v1/entities/{entity_id}"), Some(body))
                }
            }
            Phase::Spike => {
                // Spike: 90% writes to stress the write path
                let body = format!(
                    r#"{{"entity_id":{entity_id},"burst":true,"seq":{counter}}}"#
                );
                ("POST", "/api/v1/entities".to_string(), Some(body))
            }
        };

        // HMAC-sign the body (or path for GETs).
        // Crypto is natural in Rust/WASM; very painful in Lua.
        let sign_payload = body_str.as_deref().unwrap_or(&path);
        let signature = hmac_sign(&SIGNING_KEY, sign_payload.as_bytes());

        let target_idx = (counter as usize) % TARGETS.len();
        let url = format!("{}{}", TARGETS[target_idx], path);

        let mut headers = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("X-Request-ID".to_string(), format!("{}", ctx.request_id)),
            // HMAC signature header: trivial in Rust, painful in Lua
            ("X-Signature".to_string(), signature),
            // Phase indicator for server-side analysis
            ("X-Test-Phase".to_string(), match phase {
                Phase::Warmup => "warmup",
                Phase::Steady => "steady",
                Phase::Spike  => "spike",
            }.to_string()),
        ];

        if let Some(ref body) = body_str {
            headers.push(("Content-Length".to_string(), body.len().to_string()));
        }

        let spec = HttpRequestSpec {
            method: method.to_string(),
            url,
            headers,
            body: body_str.map(|s| s.into_bytes()),
        };

        OUTPUT_BUF = postcard::to_allocvec(&spec).unwrap_or_default();
        OUTPUT_BUF.len() as u32
    }
}

#[no_mangle]
pub extern "C" fn update_targets(input_len: u32) -> u32 {
    unsafe {
        let input = &INPUT_BUF[..input_len as usize];
        match postcard::from_bytes::<Vec<String>>(input) {
            Ok(targets) if !targets.is_empty() => { TARGETS = targets; 0 }
            _ => 1,
        }
    }
}
```

```bash
# Compile once
cd wasm_generator && cargo build --target wasm32-wasip1 --release

# Run
netanvil-cli test --url http://api.example.com \
  --plugin target/wasm32-wasip1/release/wasm_generator.wasm \
  --rps 35000 --duration 300s --cores 8
```

This generates a phased workload:
```
Phase: warmup (requests 0-999)
  GET  /api/v1/entities/31       X-Signature: a1b2c3...  X-Test-Phase: warmup

Phase: steady (requests 1000-4999)
  GET  /api/v1/entities/8847     X-Signature: d4e5f6...  X-Test-Phase: steady
  GET  /api/v1/entities/12003    X-Signature: 78a9b0...  X-Test-Phase: steady
  PUT  /api/v1/entities/15291    X-Signature: c1d2e3...  X-Test-Phase: steady
                                 {"entity_id":15291,"value":"data-1502",...}

Phase: spike (requests 5000+)
  POST /api/v1/entities          X-Signature: f4a5b6...  X-Test-Phase: spike
                                 {"entity_id":19847,"burst":true,"seq":5001}
```

**Key things only WASM can do well here:**
- Bloom filter for deduplication (Lua would need a huge table, hybrid impossible)
- HMAC signing of request bodies (no crypto in Lua stdlib)
- Multi-phase state machine with different read/write ratios per phase
- Written in Rust with full access to `std` and crate ecosystem
- Sandboxed: the plugin cannot access the host filesystem or network

### Building a WASM Plugin from Scratch

A minimal template is provided at `examples/wasm-plugin/`. To create your own:

**1. Create a new crate:**

```bash
cargo new --lib my-plugin
cd my-plugin
```

**2. Configure `Cargo.toml`:**

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
serde = { version = "1", features = ["derive"] }
postcard = { version = "1", default-features = false, features = ["alloc"] }

[profile.release]
opt-level = "s"
lto = true
```

**3. Implement the required exports in `src/lib.rs`:**

Your module must export five C-ABI functions: `get_input_buf_ptr`,
`get_output_buf_ptr`, `init`, `generate`, and `update_targets`. See the
template at `examples/wasm-plugin/src/lib.rs` for the full implementation.

The key function is `generate()`:

```rust
#[no_mangle]
pub extern "C" fn generate(input_len: u32) -> u32 {
    unsafe {
        // Read 24-byte binary context (zero deserialization).
        let ctx = &*(INPUT_BUF.as_ptr() as *const RawContext);

        // Build your request spec.
        let spec = HttpRequestSpec {
            method: "GET".to_string(),
            url: format!("{}?id={}", TARGETS[0], ctx.request_id),
            headers: vec![],
            body: None,
        };

        // Encode with postcard and return length.
        OUTPUT_BUF = postcard::to_allocvec(&spec).unwrap_or_default();
        OUTPUT_BUF.len() as u32
    }
}
```

**4. Build:**

```bash
rustup target add wasm32-wasip1   # one-time setup
cargo build --target wasm32-wasip1 --release
```

**5. Run:**

```bash
netanvil-cli test --url http://your-target:8080 \
  --plugin target/wasm32-wasip1/release/my_plugin.wasm \
  --rps 1000 --duration 30s
```

---

## Choosing a Plugin Type

```
Do you need per-request logic?
  |
  No --> Use HYBRID (fastest, simplest)
  |
  Yes --> Do you need crypto, complex data structures, or non-Lua languages?
            |
            No --> Use LUA (easiest to write, fast enough)
            |
            Yes --> Use WASM (most powerful, but harder workflow)
```

### Decision Matrix

| Scenario                                         | Recommended |
|--------------------------------------------------|-------------|
| Hit 3 endpoints with static headers              | Hybrid      |
| Weighted URL distribution with body template     | Hybrid      |
| Rotate through user pool with auth tokens        | Lua         |
| Mixed GET/POST based on session state             | Lua         |
| A/B test with 80/20 traffic split                | Lua         |
| Conditional request bodies                        | Lua         |
| HMAC-signed requests                              | WASM        |
| Bloom filter / HyperLogLog deduplication         | WASM        |
| Protocol buffer serialization                     | WASM        |
| Plugin written in Go, C, or Zig                   | WASM        |
| Untrusted third-party plugin                      | WASM        |
| Multi-phase test (warmup -> steady -> spike)     | Lua or WASM |

## Performance

Benchmarked with `cargo bench -p netanvil-plugin`:

| Plugin type | Per-call overhead | Relative to native | Budget used (20us) |
|-------------|-------------------|--------------------|---------------------|
| Native Rust | 203ns             | 1x                 | 1.0%                |
| Hybrid      | 603ns             | 3.0x               | 3.0%                |
| WASM        | 878ns             | 4.3x               | 4.4%                |
| LuaJIT      | 2.06us            | 10.1x              | 10.3%               |
| Rhai        | 4.2us             | 20.7x              | 21.0%               |

All plugin types leave >79% of the per-request budget for actual HTTP execution.

## Plugin API Reference

### Hybrid Plugin (Lua)

Must define a `configure()` function returning a table:

```lua
function configure()
    return {
        method = "GET",                    -- HTTP method (required)
        url_patterns = {                   -- at least one (required)
            { pattern = "http://...", weight = 1.0 },
        },
        headers = {                        -- optional
            {"Name", "Value"},
        },
        body_template = "...",             -- optional, supports {seq}, {core_id}, {request_id}
    }
end
```

### Lua Plugin (LuaJIT / Lua 5.4)

Must define a `generate(ctx)` function. Optionally define `init(targets)` and
`update_targets(targets)`.

```lua
function init(targets)         -- called once with target URLs
function generate(ctx)         -- called per request, must return spec table
function update_targets(new)   -- called when targets change mid-test
```

**ctx fields:** `request_id` (u64), `core_id` (int), `is_sampled` (bool), `session_id` (int or nil)

**return table:** `method` (string), `url` (string), `headers` (list of {key, value}), `body` (string or nil)

### WASM Plugin

Must export these C-ABI functions:

```
get_input_buf_ptr() -> i32           -- pointer to input buffer (host writes here)
get_output_buf_ptr() -> i32          -- pointer to output buffer (host reads here)
init(input_len: i32) -> i32          -- initialize with postcard-encoded targets
generate(input_len: i32) -> i32      -- generate request, returns output byte length
update_targets(input_len: i32) -> i32 -- update targets with postcard-encoded list
```

**Binary protocol:**

- **Context** (host -> guest): 24-byte `#[repr(C)]` struct written directly to the
  input buffer. Read via pointer cast — zero deserialization.

  | Offset | Type   | Field        | Description                              |
  |--------|--------|--------------|------------------------------------------|
  | 0      | u64    | request_id   | Unique request ID (partitioned by core)  |
  | 8      | u32    | core_id      | Which core is executing (0-based)        |
  | 12     | u8     | flags        | Bit 0: is_sampled, Bit 1: has_session_id |
  | 13     | [u8;3] | _pad         | Alignment padding                        |
  | 16     | u64    | session_id   | Valid only if flags bit 1 is set         |

- **Spec output** (guest -> host): `postcard`-encoded struct with fields
  `{method, url, headers, body}`. The host reads directly from guest memory.

- **Target lists** (init/update_targets): `postcard`-encoded `Vec<String>`.

See `examples/wasm-plugin/` for a minimal template, or `crates/netanvil-plugin/guest-wasm/`
for the reference implementation.
