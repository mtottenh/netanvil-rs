# Example WASM Plugin for netanvil-rs

A minimal starting template for building WASM-based request generators.

## Prerequisites

```bash
rustup target add wasm32-wasip1
```

## Build

```bash
cargo build --target wasm32-wasip1 --release
```

The compiled module is at `target/wasm32-wasip1/release/example_wasm_plugin.wasm`.

## Run

```bash
netanvil-cli test \
  --url http://your-target:8080 \
  --plugin target/wasm32-wasip1/release/example_wasm_plugin.wasm \
  --rps 1000 --duration 30s
```

## How It Works

The host (netanvil-rs) and the WASM guest communicate via shared linear memory
using a binary protocol:

1. **Context** (host -> guest): 24-byte `#[repr(C)]` struct written directly
   to the input buffer. The guest reads it via pointer cast (zero deserialization).

2. **Spec output** (guest -> host): `postcard`-encoded `HttpRequestSpec`.
   The host reads it directly from guest memory (no intermediate copy).

3. **Target lists**: `postcard`-encoded `Vec<String>` (for `init` and `update_targets`).

### RawContext Layout (24 bytes)

| Offset | Type   | Field        | Description                              |
|--------|--------|--------------|------------------------------------------|
| 0      | u64    | request_id   | Unique request ID (partitioned by core)  |
| 8      | u32    | core_id      | Which core is executing (0-based)        |
| 12     | u8     | flags        | Bit 0: is_sampled, Bit 1: has_session_id |
| 13     | [u8;3] | _pad         | Alignment padding                        |
| 16     | u64    | session_id   | Valid only if flags bit 1 is set         |

### Required Exports

Your WASM module must export these C-ABI functions:

| Function                          | Description                                    |
|-----------------------------------|------------------------------------------------|
| `get_input_buf_ptr() -> i32`      | Pointer to input buffer (host writes here)     |
| `get_output_buf_ptr() -> i32`     | Pointer to output buffer (host reads from here)|
| `init(input_len: i32) -> i32`     | Initialize with target URLs. Return 0 = success|
| `generate(input_len: i32) -> i32` | Generate request. Return output byte length    |
| `update_targets(input_len: i32) -> i32` | Update targets mid-test. Return 0 = success |

## Customizing

Edit `src/lib.rs` — modify the `generate()` function to implement your request
generation logic. You have access to the full Rust standard library and any crate
that compiles to `wasm32-wasip1`.

Common modifications:
- Change HTTP method, headers, or body based on request context
- Add stateful logic (counters, session tracking, rate limiting)
- Use external crates for crypto (HMAC, JWT), compression, or serialization
- Implement multi-phase test patterns (warmup -> steady -> spike)

See `docs/plugins.md` for a full-featured example with HMAC signing and bloom filters.
