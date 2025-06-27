//! WASM guest module implementing RequestGenerator.
//!
//! This is compiled to wasm32-wasip1 and loaded by WasmGenerator at runtime.
//! Communication with the host uses JSON serialization over linear memory.
//!
//! Protocol:
//! 1. Host writes JSON RequestContext to guest memory at the input buffer
//! 2. Host calls `generate(input_ptr, input_len)` -> output_len
//! 3. Host reads JSON RequestSpec from guest memory at the output buffer
//!
//! The guest maintains internal state (counter, targets) across calls.

use serde::{Deserialize, Serialize};

/// Mirrors netanvil_types::RequestContext (minus Instant fields which aren't serializable)
#[derive(Deserialize)]
struct RequestContext {
    request_id: u64,
    core_id: usize,
    is_sampled: bool,
    session_id: Option<u64>,
}

/// Mirrors netanvil_types::RequestSpec
#[derive(Serialize)]
struct RequestSpec {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
}

// ---------------------------------------------------------------------------
// Guest state (persists across calls via WASM globals)
// ---------------------------------------------------------------------------

static mut COUNTER: u64 = 0;
static mut TARGETS: Vec<String> = Vec::new();
static mut OUTPUT_BUF: Vec<u8> = Vec::new();
static mut INITIALIZED: bool = false;

// 64KB input buffer at a known location
const INPUT_BUF_SIZE: usize = 65536;
static mut INPUT_BUF: [u8; INPUT_BUF_SIZE] = [0u8; INPUT_BUF_SIZE];

/// Returns pointer to the input buffer where the host should write JSON.
#[no_mangle]
pub extern "C" fn get_input_buf_ptr() -> *mut u8 {
    unsafe { INPUT_BUF.as_mut_ptr() }
}

/// Initialize the generator with a JSON array of target URLs.
/// Host writes JSON to input buffer, then calls this with the length.
#[no_mangle]
pub extern "C" fn init(input_len: u32) -> u32 {
    unsafe {
        let input = &INPUT_BUF[..input_len as usize];
        match serde_json::from_slice::<Vec<String>>(input) {
            Ok(targets) => {
                TARGETS = targets;
                COUNTER = 0;
                INITIALIZED = true;
                0 // success
            }
            Err(_) => 1, // error
        }
    }
}

/// Generate a request. Host writes JSON RequestContext to input buffer,
/// calls this function, then reads the output.
///
/// Returns: length of JSON output (read from get_output_buf_ptr)
#[no_mangle]
pub extern "C" fn generate(input_len: u32) -> u32 {
    unsafe {
        if !INITIALIZED || TARGETS.is_empty() {
            return 0;
        }

        let input = &INPUT_BUF[..input_len as usize];
        let ctx: RequestContext = match serde_json::from_slice(input) {
            Ok(c) => c,
            Err(_) => return 0,
        };

        let target_idx = (COUNTER as usize) % TARGETS.len();
        let url = format!(
            "{}?seq={}&core={}",
            TARGETS[target_idx], COUNTER, ctx.core_id
        );

        let body = format!(
            r#"{{"request_id":{},"seq":{},"core_id":{}}}"#,
            ctx.request_id, COUNTER, ctx.core_id
        );

        COUNTER += 1;

        let spec = RequestSpec {
            method: "GET".to_string(),
            url,
            headers: vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                ("X-Request-ID".to_string(), format!("{}", ctx.request_id)),
            ],
            body: Some(body.into_bytes()),
        };

        OUTPUT_BUF = serde_json::to_vec(&spec).unwrap_or_default();
        OUTPUT_BUF.len() as u32
    }
}

/// Returns pointer to the output buffer where the host reads the result.
#[no_mangle]
pub extern "C" fn get_output_buf_ptr() -> *const u8 {
    unsafe { OUTPUT_BUF.as_ptr() }
}

/// Update targets mid-test. Same protocol as init.
#[no_mangle]
pub extern "C" fn update_targets(input_len: u32) -> u32 {
    unsafe {
        let input = &INPUT_BUF[..input_len as usize];
        match serde_json::from_slice::<Vec<String>>(input) {
            Ok(targets) if !targets.is_empty() => {
                TARGETS = targets;
                COUNTER = 0;
                0
            }
            _ => 1,
        }
    }
}
