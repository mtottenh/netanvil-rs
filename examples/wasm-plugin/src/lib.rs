//! Minimal WASM plugin example for netanvil-rs.
//!
//! This is a starting template for building WASM-based request generators.
//! Copy this directory, modify `generate()`, and compile:
//!
//!   cargo build --target wasm32-wasip1 --release
//!
//! Then run:
//!
//!   netanvil-cli test --url http://your-target:8080 \
//!     --plugin target/wasm32-wasip1/release/example_wasm_plugin.wasm \
//!     --rps 1000 --duration 30s
//!
//! ## Protocol
//!
//! The host communicates with the guest via shared linear memory:
//!
//! **Context input** (host -> guest): A 24-byte `#[repr(C)]` struct written
//! directly to the input buffer. Read it via pointer cast — zero deserialization.
//!
//! **Spec output** (guest -> host): A `postcard`-encoded `HttpRequestSpec` struct.
//! The host reads it directly from guest memory.
//!
//! **Target lists** (init/update_targets): `postcard`-encoded `Vec<String>`.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Context struct — must match the host's RawContext layout exactly.
// ---------------------------------------------------------------------------

/// 24-byte context passed from the host for every request.
/// Fields match `netanvil_plugin::types::RawContext`.
#[repr(C)]
#[derive(Clone, Copy)]
struct RawContext {
    /// Unique request ID (partitioned by core).
    request_id: u64, // offset 0
    /// Which core is executing this request (0-based).
    core_id: u32, // offset 8
    /// Bit 0: is_sampled, Bit 1: has_session_id.
    flags: u8, // offset 12
    _pad: [u8; 3], // offset 13
    /// Session ID (valid only if flags bit 1 is set).
    session_id: u64, // offset 16
}

// Compile-time check: layout must be exactly 24 bytes.
const _: () = assert!(core::mem::size_of::<RawContext>() == 24);

// ---------------------------------------------------------------------------
// Output spec — the request to generate.
// ---------------------------------------------------------------------------

/// The request specification returned to the host.
/// Fields mirror `netanvil_types::HttpRequestSpec`.
#[derive(Serialize, Deserialize)]
struct HttpRequestSpec {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
}

// ---------------------------------------------------------------------------
// Guest state — persists across calls via WASM globals.
// ---------------------------------------------------------------------------

static mut COUNTER: u64 = 0;
static mut TARGETS: Vec<String> = Vec::new();
static mut OUTPUT_BUF: Vec<u8> = Vec::new();
static mut INITIALIZED: bool = false;

const INPUT_BUF_SIZE: usize = 65536;
static mut INPUT_BUF: [u8; INPUT_BUF_SIZE] = [0u8; INPUT_BUF_SIZE];

// ---------------------------------------------------------------------------
// Required exports — the host calls these via the WASM C ABI.
// ---------------------------------------------------------------------------

/// Returns pointer to the input buffer (host writes context/targets here).
#[no_mangle]
pub extern "C" fn get_input_buf_ptr() -> *mut u8 {
    unsafe { INPUT_BUF.as_mut_ptr() }
}

/// Returns pointer to the output buffer (host reads the spec from here).
#[no_mangle]
pub extern "C" fn get_output_buf_ptr() -> *const u8 {
    unsafe { OUTPUT_BUF.as_ptr() }
}

/// Initialize with postcard-encoded target URLs. Returns 0 on success.
#[no_mangle]
pub extern "C" fn init(input_len: u32) -> u32 {
    unsafe {
        let input = &INPUT_BUF[..input_len as usize];
        match postcard::from_bytes::<Vec<String>>(input) {
            Ok(targets) => {
                TARGETS = targets;
                COUNTER = 0;
                INITIALIZED = true;
                0
            }
            Err(_) => 1,
        }
    }
}

/// Generate a request.
///
/// The host writes a 24-byte `RawContext` to the input buffer, then calls
/// this function. The guest writes a postcard-encoded `HttpRequestSpec` to
/// the output buffer and returns its length.
#[no_mangle]
pub extern "C" fn generate(input_len: u32) -> u32 {
    unsafe {
        if !INITIALIZED || TARGETS.is_empty() {
            return 0;
        }

        // ---- Read context (zero-copy from shared memory) ----
        if (input_len as usize) < core::mem::size_of::<RawContext>() {
            return 0;
        }
        let ctx = &*(INPUT_BUF.as_ptr() as *const RawContext);

        // ---- Your request generation logic goes here ----

        let target_idx = (COUNTER as usize) % TARGETS.len();
        let url = format!(
            "{}?seq={}&core={}",
            TARGETS[target_idx], COUNTER, ctx.core_id
        );

        COUNTER += 1;

        let spec = HttpRequestSpec {
            method: "GET".to_string(),
            url,
            headers: vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                (
                    "X-Request-ID".to_string(),
                    format!("{}", ctx.request_id),
                ),
            ],
            body: None,
        };

        // ---- Encode output with postcard ----
        OUTPUT_BUF = postcard::to_allocvec(&spec).unwrap_or_default();
        OUTPUT_BUF.len() as u32
    }
}

/// Update targets mid-test. Receives postcard-encoded `Vec<String>`.
#[no_mangle]
pub extern "C" fn update_targets(input_len: u32) -> u32 {
    unsafe {
        let input = &INPUT_BUF[..input_len as usize];
        match postcard::from_bytes::<Vec<String>>(input) {
            Ok(targets) if !targets.is_empty() => {
                TARGETS = targets;
                COUNTER = 0;
                0
            }
            _ => 1,
        }
    }
}
