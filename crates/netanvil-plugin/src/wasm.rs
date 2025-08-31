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
use crate::types::{FromPostcard, RawContext};

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

        let mut gen = Self {
            store,
            memory,
            fn_generate,
            fn_get_input_ptr,
            fn_get_output_ptr,
            fn_update_targets,
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
