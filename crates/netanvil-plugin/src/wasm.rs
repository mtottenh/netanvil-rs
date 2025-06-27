//! WASM plugin runtime using wasmtime.
//!
//! Loads a .wasm module compiled from Rust (or any language targeting wasm32-wasip1)
//! and calls its `generate()` function on each request. Data is marshaled as JSON
//! through WASM linear memory.
//!
//! # Instance-per-core
//!
//! Each `WasmGenerator` owns its own `Store` and `Instance`. Since our workers
//! are `!Send`, this is safe — one instance per core, no sharing needed.
//! Instance creation from a pre-compiled `Module` is cheap (~microseconds).

use wasmtime::*;
use wasmtime_wasi::preview1::WasiP1Ctx;
use wasmtime_wasi::WasiCtxBuilder;

use crate::error::{PluginError, Result};
use crate::types::{PluginRequestContext, PluginRequestSpec};

/// A RequestGenerator backed by a WASM module.
///
/// The WASM module must export:
/// - `get_input_buf_ptr() -> i32` — pointer to input buffer in linear memory
/// - `get_output_buf_ptr() -> i32` — pointer to output buffer in linear memory
/// - `init(input_len: i32) -> i32` — initialize with JSON target URLs
/// - `generate(input_len: i32) -> i32` — generate request, returns output JSON length
/// - `update_targets(input_len: i32) -> i32` — update targets mid-test
pub struct WasmGenerator {
    store: Store<WasiP1Ctx>,
    memory: Memory,
    fn_generate: TypedFunc<i32, i32>,
    fn_get_input_ptr: TypedFunc<(), i32>,
    fn_get_output_ptr: TypedFunc<(), i32>,
    fn_update_targets: TypedFunc<i32, i32>,
    /// Pre-allocated buffer for serialized context to avoid per-call allocation.
    ctx_buf: Vec<u8>,
}

impl WasmGenerator {
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

        let mut gen = Self {
            store,
            memory,
            fn_generate,
            fn_get_input_ptr,
            fn_get_output_ptr,
            fn_update_targets,
            ctx_buf: Vec::with_capacity(256),
        };

        // Initialize the WASM module with targets
        let targets_json = serde_json::to_vec(targets)?;
        gen.write_to_guest(&targets_json)?;
        let rc = fn_init
            .call(&mut gen.store, targets_json.len() as i32)
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

impl netanvil_types::RequestGenerator for WasmGenerator {
    fn generate(&mut self, context: &netanvil_types::RequestContext) -> netanvil_types::RequestSpec {
        // Serialize context to JSON
        let plugin_ctx = PluginRequestContext::from(context);
        self.ctx_buf.clear();
        serde_json::to_writer(&mut self.ctx_buf, &plugin_ctx)
            .expect("context serialization cannot fail");

        // Write context JSON to guest input buffer
        let input_ptr = self
            .fn_get_input_ptr
            .call(&mut self.store, ())
            .expect("get_input_buf_ptr trap") as usize;

        let ctx_len = self.ctx_buf.len();
        self.memory.data_mut(&mut self.store)[input_ptr..input_ptr + ctx_len]
            .copy_from_slice(&self.ctx_buf);

        // Call generate(ctx_len) -> output_len
        let output_len = self
            .fn_generate
            .call(&mut self.store, ctx_len as i32)
            .expect("generate trap");

        if output_len <= 0 {
            return fallback_spec();
        }

        // Read output JSON from guest output buffer
        let output_ptr = self
            .fn_get_output_ptr
            .call(&mut self.store, ())
            .expect("get_output_buf_ptr trap") as usize;
        let output_len = output_len as usize;

        let output_bytes =
            self.memory.data(&self.store)[output_ptr..output_ptr + output_len].to_vec();

        match serde_json::from_slice::<PluginRequestSpec>(&output_bytes) {
            Ok(spec) => spec.into_request_spec(),
            Err(_) => fallback_spec(),
        }
    }

    fn update_targets(&mut self, targets: Vec<String>) {
        if let Ok(json) = serde_json::to_vec(&targets) {
            if self.write_to_guest(&json).is_ok() {
                let _ = self
                    .fn_update_targets
                    .call(&mut self.store, json.len() as i32);
            }
        }
    }
}

fn fallback_spec() -> netanvil_types::RequestSpec {
    netanvil_types::RequestSpec {
        method: http::Method::GET,
        url: "http://error.invalid".into(),
        headers: vec![],
        body: None,
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
