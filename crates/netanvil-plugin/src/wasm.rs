//! WASM plugin runtime using wasmtime.
//!
//! Loads a .wasm module compiled from Rust (or any language targeting wasm32-wasip1)
//! and calls its `generate()` function on each request.
//!
//! ## Binary protocol
//!
//! Context is passed as a 24-byte `#[repr(C)]` struct (`RawContext`) — zero
//! serialization overhead. The spec output is encoded with `postcard` (compact
//! serde binary format) and read directly from WASM linear memory without
//! intermediate allocation.
//!
//! ## Instance-per-core
//!
//! Each `WasmGenerator` owns its own `Store` and `Instance`. Since our workers
//! are `!Send`, this is safe — one instance per core, no sharing needed.
//! Instance creation from a pre-compiled `Module` is cheap (~microseconds).

use wasmtime::*;
use wasmtime_wasi::preview1::WasiP1Ctx;
use wasmtime_wasi::WasiCtxBuilder;

use std::marker::PhantomData;

use crate::error::{PluginError, Result};
use crate::types::{
    FromPostcard, RawContext, RawResult, ResponseConfig, ResponseVarData, RAW_RESULT_HAS_VAR_DATA,
};

/// A RequestGenerator backed by a WASM module.
///
/// The WASM module must export:
/// - `get_input_buf_ptr() -> i32` — pointer to input buffer in linear memory
/// - `get_output_buf_ptr() -> i32` — pointer to output buffer in linear memory
/// - `init(input_len: i32) -> i32` — initialize with JSON target URLs
/// - `generate(input_len: i32) -> i32` — generate request, returns output JSON length
/// - `update_targets(input_len: i32) -> i32` — update targets mid-test
pub struct WasmGenerator<S: FromPostcard> {
    store: Store<WasiP1Ctx>,
    memory: Memory,
    fn_generate: TypedFunc<i32, i32>,
    fn_get_input_ptr: TypedFunc<(), i32>,
    fn_get_output_ptr: TypedFunc<(), i32>,
    fn_update_targets: TypedFunc<i32, i32>,
    /// Optional `on_response(input_len: i32) -> i32` WASM export.
    fn_on_response: Option<TypedFunc<i32, i32>>,
    /// What response data the plugin wants (from `response_config()` export).
    response_cfg: ResponseConfig,
    /// Pre-allocated host buffer for building response payloads.
    /// Reused per call — after warmup, no allocations occur.
    response_buf: Vec<u8>,
    _phantom: PhantomData<S>,
}

impl<S: FromPostcard> WasmGenerator<S> {
    /// Create a new WasmGenerator from a pre-compiled WASM module.
    ///
    /// `targets` are the initial target URLs passed to the WASM `init()` function.
    /// The `Engine` should be shared across all cores (it's thread-safe and holds
    /// the compiled code). Each core creates its own `WasmGenerator` with its own Store.
    pub fn new(engine: &Engine, module: &Module, targets: &[String]) -> Result<Self> {
        let wasi_ctx = WasiCtxBuilder::new().build_p1();
        let mut store = Store::new(engine, wasi_ctx);
        let mut linker = Linker::new(engine);
        wasmtime_wasi::preview1::add_to_linker_sync(&mut linker, |ctx| ctx)
            .map_err(|e| PluginError::Wasm(format!("WASI link: {e}")))?;
        let instance = linker
            .instantiate(&mut store, module)
            .map_err(|e| PluginError::Wasm(format!("instantiation: {e}")))?;

        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| PluginError::Wasm("no 'memory' export".into()))?;

        let fn_get_input_ptr = instance
            .get_typed_func::<(), i32>(&mut store, "get_input_buf_ptr")
            .map_err(|e| PluginError::Wasm(format!("missing get_input_buf_ptr: {e}")))?;

        let fn_get_output_ptr = instance
            .get_typed_func::<(), i32>(&mut store, "get_output_buf_ptr")
            .map_err(|e| PluginError::Wasm(format!("missing get_output_buf_ptr: {e}")))?;

        let fn_init = instance
            .get_typed_func::<i32, i32>(&mut store, "init")
            .map_err(|e| PluginError::Wasm(format!("missing init: {e}")))?;

        let fn_generate = instance
            .get_typed_func::<i32, i32>(&mut store, "generate")
            .map_err(|e| PluginError::Wasm(format!("missing generate: {e}")))?;

        let fn_update_targets = instance
            .get_typed_func::<i32, i32>(&mut store, "update_targets")
            .map_err(|e| PluginError::Wasm(format!("missing update_targets: {e}")))?;

        // Optional: on_response export for response feedback
        let fn_on_response = instance
            .get_typed_func::<i32, i32>(&mut store, "on_response")
            .ok();

        // Probe for response_config() export to determine what data the plugin needs.
        // Returns a flags byte: bit 0 = headers, bit 1 = body.
        let response_cfg = if fn_on_response.is_some() {
            if let Ok(config_fn) = instance.get_typed_func::<(), i32>(&mut store, "response_config")
            {
                let flags = config_fn.call(&mut store, ()).unwrap_or(0x01); // default: headers only
                ResponseConfig {
                    headers: flags & 0x01 != 0,
                    body: flags & 0x02 != 0,
                }
            } else {
                ResponseConfig::on_response_default()
            }
        } else {
            ResponseConfig::default()
        };

        let mut gen = Self {
            store,
            memory,
            fn_generate,
            fn_get_input_ptr,
            fn_get_output_ptr,
            fn_update_targets,
            fn_on_response,
            response_cfg,
            response_buf: Vec::with_capacity(4096),
            _phantom: PhantomData,
        };

        // Initialize the WASM module with targets (postcard-encoded).
        let targets_bytes = postcard::to_allocvec(targets)
            .map_err(|e| PluginError::Wasm(format!("targets encode: {e}")))?;
        gen.write_to_guest(&targets_bytes)?;
        let rc = fn_init
            .call(&mut gen.store, targets_bytes.len() as i32)
            .map_err(|e| PluginError::Wasm(format!("init call: {e}")))?;
        if rc != 0 {
            return Err(PluginError::Init("WASM init returned error".into()));
        }

        Ok(gen)
    }

    /// Write data into the guest's input buffer.
    fn write_to_guest(&mut self, data: &[u8]) -> Result<()> {
        let ptr = self
            .fn_get_input_ptr
            .call(&mut self.store, ())
            .map_err(|e| PluginError::Wasm(format!("get_input_buf_ptr: {e}")))?
            as usize;

        let mem = self.memory.data_mut(&mut self.store);
        if ptr + data.len() > mem.len() {
            return Err(PluginError::Wasm("input exceeds guest buffer".into()));
        }
        mem[ptr..ptr + data.len()].copy_from_slice(data);
        Ok(())
    }
}

impl<S: FromPostcard> netanvil_types::RequestGenerator for WasmGenerator<S> {
    type Spec = S;

    fn generate(&mut self, context: &netanvil_types::RequestContext) -> S {
        // Write 24-byte repr(C) context directly into guest memory (zero serialization).
        let raw = RawContext::from_context(context);
        let input_ptr = self
            .fn_get_input_ptr
            .call(&mut self.store, ())
            .expect("get_input_buf_ptr trap") as usize;
        self.memory.data_mut(&mut self.store)[input_ptr..input_ptr + 24]
            .copy_from_slice(raw.as_bytes());

        // Call generate(24) -> output_len
        let output_len = self
            .fn_generate
            .call(&mut self.store, 24)
            .expect("generate trap");

        if output_len <= 0 {
            return S::fallback();
        }

        // Read postcard-encoded spec directly from guest memory (no .to_vec() allocation).
        let output_ptr = self
            .fn_get_output_ptr
            .call(&mut self.store, ())
            .expect("get_output_buf_ptr trap") as usize;
        let output_len = output_len as usize;
        let output_slice = &self.memory.data(&self.store)[output_ptr..output_ptr + output_len];

        S::from_postcard_bytes(output_slice).unwrap_or_else(|_| S::fallback())
    }

    fn on_response(&mut self, result: &netanvil_types::ExecutionResult) {
        if self.fn_on_response.is_none() {
            return;
        }

        // Build response into pre-allocated buffer (amortized zero alloc after warmup).
        self.response_buf.clear();

        // Fixed fields: 40-byte repr(C) struct, zero serialization.
        let mut raw = RawResult::from_execution_result(result);

        // Variable data: only include what response_config() requested.
        let needs_var = result.error.is_some()
            || (self.response_cfg.headers && result.response_headers.is_some())
            || (self.response_cfg.body && result.response_body.is_some());

        if needs_var {
            raw.flags |= RAW_RESULT_HAS_VAR_DATA;
            let var_data = ResponseVarData {
                error: result.error.as_ref().map(|e| e.to_string()),
                headers: if self.response_cfg.headers {
                    result.response_headers.clone().unwrap_or_default()
                } else {
                    vec![]
                },
                body: if self.response_cfg.body {
                    result.response_body.as_ref().map(|b| b.to_vec())
                } else {
                    None
                },
            };

            self.response_buf.extend_from_slice(raw.as_bytes());
            if let Ok(var_bytes) = postcard::to_allocvec(&var_data) {
                self.response_buf.extend_from_slice(&var_bytes);
            }
        } else {
            // Common fast path: just the 40-byte fixed struct, no variable data.
            self.response_buf.extend_from_slice(raw.as_bytes());
        }

        // Write to guest memory — inline to avoid borrow conflict with self.response_buf.
        let buf_len = self.response_buf.len();
        let write_ok = {
            let ptr = self
                .fn_get_input_ptr
                .call(&mut self.store, ())
                .map(|p| p as usize);
            if let Ok(ptr) = ptr {
                let mem = self.memory.data_mut(&mut self.store);
                if ptr + buf_len <= mem.len() {
                    mem[ptr..ptr + buf_len].copy_from_slice(&self.response_buf);
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };
        if write_ok {
            let func = self.fn_on_response.clone().unwrap();
            let _ = func.call(&mut self.store, buf_len as i32);
        }
    }

    fn wants_responses(&self) -> bool {
        self.fn_on_response.is_some()
    }

    fn update_targets(&mut self, targets: Vec<String>) {
        if let Ok(bytes) = postcard::to_allocvec(&targets) {
            if self.write_to_guest(&bytes).is_ok() {
                let _ = self
                    .fn_update_targets
                    .call(&mut self.store, bytes.len() as i32);
            }
        }
    }
}

/// Pre-compile a WASM module. Returns engine + module for creating per-core instances.
///
/// The Engine is thread-safe and holds JIT-compiled native code.
/// Call `WasmGenerator::new(&engine, &module, targets)` per core.
pub fn compile_wasm_module(wasm_bytes: &[u8]) -> Result<(Engine, Module)> {
    let mut config = Config::new();
    config.cranelift_opt_level(OptLevel::Speed);

    let engine = Engine::new(&config).map_err(|e| PluginError::Wasm(format!("engine: {e}")))?;
    let module =
        Module::new(&engine, wasm_bytes).map_err(|e| PluginError::Wasm(format!("compile: {e}")))?;

    Ok((engine, module))
}
