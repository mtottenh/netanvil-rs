//! WASM plugin example with on_response callback for netanvil-rs benchmarks.
//!
//! Demonstrates the response feedback protocol:
//! - `response_config() -> i32`: returns flags (bit 0 = headers, bit 1 = body)
//! - `on_response(input_len: i32) -> i32`: receives RawResult + optional var data
//!
//! Guest memory layout for on_response input:
//!   [RawResult: 40 bytes][optional postcard ResponseVarData: N bytes]
//!
//! RawResult is repr(C), read via pointer cast (zero deserialization).

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Context struct — matches host's RawContext (24 bytes).
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
struct RawContext {
    request_id: u64,
    core_id: u32,
    flags: u8,
    _pad: [u8; 3],
    session_id: u64,
}

const _: () = assert!(core::mem::size_of::<RawContext>() == 24);

// ---------------------------------------------------------------------------
// RawResult struct — matches host's RawResult (40 bytes).
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
struct RawResult {
    request_id: u64,  // 0
    status: u16,      // 8
    flags: u8,        // 10
    _pad: [u8; 5],    // 11
    latency_ns: u64,  // 16
    bytes_sent: u64,  // 24
    response_size: u64, // 32
}

const _: () = assert!(core::mem::size_of::<RawResult>() == 40);

const RAW_RESULT_HAS_ERROR: u8 = 0x01;
const RAW_RESULT_HAS_VAR_DATA: u8 = 0x02;

// ---------------------------------------------------------------------------
// Variable response data (postcard-encoded after RawResult when present).
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ResponseVarData {
    error: Option<String>,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
}

// ---------------------------------------------------------------------------
// Output spec for generate().
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct HttpRequestSpec {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
}

// ---------------------------------------------------------------------------
// Guest state.
// ---------------------------------------------------------------------------

static mut COUNTER: u64 = 0;
static mut TARGETS: Vec<String> = Vec::new();
static mut OUTPUT_BUF: Vec<u8> = Vec::new();
static mut INITIALIZED: bool = false;
static mut OK_COUNT: u64 = 0;
static mut TOKEN: Option<String> = None;

const INPUT_BUF_SIZE: usize = 65536;
static mut INPUT_BUF: [u8; INPUT_BUF_SIZE] = [0u8; INPUT_BUF_SIZE];

// ---------------------------------------------------------------------------
// Required exports.
// ---------------------------------------------------------------------------

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
                OK_COUNT = 0;
                TOKEN = None;
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
        if (input_len as usize) < core::mem::size_of::<RawContext>() {
            return 0;
        }
        let ctx = &*(INPUT_BUF.as_ptr() as *const RawContext);

        let target_idx = (COUNTER as usize) % TARGETS.len();
        let url = format!(
            "{}?seq={}&core={}",
            TARGETS[target_idx], COUNTER, ctx.core_id
        );
        COUNTER += 1;

        let mut headers = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
        ];
        if let Some(ref tok) = TOKEN {
            headers.push(("Authorization".to_string(), format!("Bearer {tok}")));
        }

        let spec = HttpRequestSpec {
            method: "GET".to_string(),
            url,
            headers,
            body: None,
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
            Ok(targets) if !targets.is_empty() => {
                TARGETS = targets;
                COUNTER = 0;
                0
            }
            _ => 1,
        }
    }
}

// ---------------------------------------------------------------------------
// Response callback exports.
// ---------------------------------------------------------------------------

/// Declare what response data we need. Returns flags byte:
/// bit 0 = headers, bit 1 = body.
#[no_mangle]
pub extern "C" fn response_config() -> u32 {
    0x01 // headers only, no body
}

/// Called by the host after each response completes.
///
/// Input buffer contains:
///   [RawResult: 40 bytes][optional postcard ResponseVarData]
///
/// RawResult.flags bit 1 indicates whether var data follows.
#[no_mangle]
pub extern "C" fn on_response(input_len: u32) -> u32 {
    unsafe {
        let len = input_len as usize;
        if len < core::mem::size_of::<RawResult>() {
            return 1;
        }

        // Read fixed fields via pointer cast (zero deserialization).
        let raw = &*(INPUT_BUF.as_ptr() as *const RawResult);

        if raw.status == 200 {
            OK_COUNT += 1;
        }

        // Read variable data if present.
        if raw.flags & RAW_RESULT_HAS_VAR_DATA != 0 && len > 40 {
            if let Ok(var) = postcard::from_bytes::<ResponseVarData>(&INPUT_BUF[40..len]) {
                // Extract session token from headers
                for (key, value) in &var.headers {
                    if key == "X-Session-Token" {
                        TOKEN = Some(value.clone());
                    }
                }
            }
        }

        0
    }
}
