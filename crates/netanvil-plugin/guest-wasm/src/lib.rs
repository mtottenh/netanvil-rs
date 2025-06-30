//! WASM guest module implementing RequestGenerator.
//!
//! This is compiled to wasm32-wasip1 and loaded by WasmGenerator at runtime.
//!
//! ## Binary protocol
//!
//! Context is received as a 24-byte `#[repr(C)]` struct (`RawContext`) — the
//! host writes it directly into WASM linear memory, and the guest reads it
//! via pointer cast (zero deserialization).
//!
//! The spec output is encoded with `postcard` (compact serde binary format),
//! which the host reads directly from linear memory without intermediate
//! allocation.
//!
//! The guest maintains internal state (counter, targets) across calls.

use serde::{Deserialize, Serialize};

/// Fixed-layout context received from the host.
/// Must match the host's `RawContext` layout exactly.
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

/// Mirrors netanvil_types::HttpRequestSpec (serializable via postcard).
#[derive(Serialize, Deserialize)]
struct HttpRequestSpec {
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

/// Returns pointer to the input buffer where the host writes data.
#[no_mangle]
pub extern "C" fn get_input_buf_ptr() -> *mut u8 {
    unsafe { INPUT_BUF.as_mut_ptr() }
}

/// Initialize the generator with a postcard-encoded array of target URLs.
/// Host writes encoded data to input buffer, then calls this with the length.
#[no_mangle]
pub extern "C" fn init(input_len: u32) -> u32 {
    unsafe {
        let input = &INPUT_BUF[..input_len as usize];
        match postcard::from_bytes::<Vec<String>>(input) {
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

/// Generate a request. Host writes 24-byte RawContext to input buffer,
/// calls this function, then reads postcard output.
///
/// Returns: length of postcard output (read from get_output_buf_ptr)
#[no_mangle]
pub extern "C" fn generate(input_len: u32) -> u32 {
    unsafe {
        if !INITIALIZED || TARGETS.is_empty() {
            return 0;
        }

        // Read binary context directly (zero deserialization).
        if (input_len as usize) < core::mem::size_of::<RawContext>() {
            return 0;
        }
        let raw = &*(INPUT_BUF.as_ptr() as *const RawContext);
        let request_id = raw.request_id;
        let core_id = raw.core_id;

        let target_idx = (COUNTER as usize) % TARGETS.len();
        let url = format!(
            "{}?seq={}&core={}",
            TARGETS[target_idx], COUNTER, core_id
        );

        let body = format!(
            r#"{{"request_id":{},"seq":{},"core_id":{}}}"#,
            request_id, COUNTER, core_id
        );

        COUNTER += 1;

        let spec = HttpRequestSpec {
            method: "GET".to_string(),
            url,
            headers: vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                ("X-Request-ID".to_string(), format!("{}", request_id)),
            ],
            body: Some(body.into_bytes()),
        };

        // Encode with postcard (was: serde_json::to_vec).
        OUTPUT_BUF = postcard::to_allocvec(&spec).unwrap_or_default();
        OUTPUT_BUF.len() as u32
    }
}

/// Returns pointer to the output buffer where the host reads the result.
#[no_mangle]
pub extern "C" fn get_output_buf_ptr() -> *const u8 {
    unsafe { OUTPUT_BUF.as_ptr() }
}

/// Update targets mid-test. Receives postcard-encoded target list.
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
