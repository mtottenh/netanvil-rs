//! Plugin system for netanvil-rs.
//!
//! Provides multiple runtime backends for user-authored request generators
//! without requiring Rust compilation:
//!
//! - **WASM** (`wasm`): WebAssembly modules via wasmtime. Best performance
//!   for complex per-request logic. Pre-compiled, sandboxed, portable.
//!
//! - **Rhai** (`rhai_runtime`): Rhai scripts. Pure Rust, zero unsafe,
//!   familiar syntax. Suitable for configuration; marginal for hot path.
//!
//! - **Hybrid** (`hybrid`): Script defines configuration at setup time,
//!   native Rust executes the hot path. Zero per-request overhead.
//!
//! Lua runtimes are in separate crates to allow simultaneous LuaJIT + Lua 5.4:
//! - `netanvil-plugin-luajit` — LuaJIT (~2.1μs/call)
//! - `netanvil-plugin-lua54` — Lua 5.4 (~2.9μs/call)
//!
//! # Shared types
//!
//! The `types` and `error` modules are public so that Lua crates can reuse
//! `PluginRequestContext`, `PluginHttpRequestSpec`, and `PluginError`.

pub mod error;
pub mod hybrid;
pub mod rhai_runtime;
pub mod types;
pub mod wasm;

pub use error::{PluginError, Result};
pub use hybrid::{GeneratorConfig, HybridGenerator, WeightedPattern};
pub use rhai_runtime::{FromRhaiPlugin, RhaiGenerator};
pub use types::{FromPostcard, ResponseConfig};
pub use wasm::{compile_wasm_module, WasmGenerator};
