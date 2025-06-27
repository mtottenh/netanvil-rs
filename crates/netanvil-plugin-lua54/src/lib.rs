//! Lua 5.4 plugin runtime for netanvil-rs.
//!
//! Provides `Lua54Generator` — a `RequestGenerator` backed by Lua 5.4 via mlua.
//! Lua 5.4 is slower than LuaJIT (~2.9μs vs ~2.1μs) but more portable and
//! supports modern Lua features (integers, utf8, generational GC).
//!
//! Also provides `config_from_lua` for the hybrid approach (Lua config → native execution).

use mlua::prelude::*;

use netanvil_plugin::error::{PluginError, Result};
use netanvil_plugin::hybrid::{GeneratorConfig, WeightedPattern};
use netanvil_plugin::types::{PluginRequestContext, PluginRequestSpec};

/// A RequestGenerator backed by Lua 5.4.
///
/// Same API as `LuaJitGenerator` but links against PUC Lua 5.4.
pub struct Lua54Generator {
    lua: Lua,
}

impl Lua54Generator {
    /// Create a new Lua54Generator from Lua source code.
    pub fn new(script: &str, targets: &[String]) -> Result<Self> {
        let lua = Lua::new();

        lua.load(script)
            .exec()
            .map_err(|e| PluginError::Lua(format!("script load: {e}")))?;

        let globals = lua.globals();
        if let Ok(init_fn) = globals.get::<LuaFunction>("init") {
            let targets_table = lua
                .create_sequence_from(targets.iter().map(|s| s.as_str()))
                .map_err(|e| PluginError::Lua(format!("create targets table: {e}")))?;
            init_fn
                .call::<()>(targets_table)
                .map_err(|e| PluginError::Lua(format!("init() call: {e}")))?;
        }

        Ok(Self { lua })
    }

    fn ctx_to_table(&self, ctx: &PluginRequestContext) -> LuaResult<LuaTable> {
        let table = self.lua.create_table()?;
        table.set("request_id", ctx.request_id)?;
        table.set("core_id", ctx.core_id)?;
        table.set("is_sampled", ctx.is_sampled)?;
        match ctx.session_id {
            Some(id) => table.set("session_id", id)?,
            None => table.set("session_id", LuaNil)?,
        }
        Ok(table)
    }

    fn table_to_spec(&self, table: LuaTable) -> LuaResult<PluginRequestSpec> {
        let method: String = table.get("method")?;
        let url: String = table.get("url")?;

        let headers: Vec<(String, String)> = match table.get::<LuaTable>("headers") {
            Ok(h) => {
                let mut headers = Vec::new();
                for pair in h.sequence_values::<LuaTable>() {
                    let pair = pair?;
                    let k: String = pair.get(1)?;
                    let v: String = pair.get(2)?;
                    headers.push((k, v));
                }
                headers
            }
            Err(_) => Vec::new(),
        };

        let body: Option<Vec<u8>> = match table.get::<LuaValue>("body") {
            Ok(LuaValue::String(s)) => Some(s.as_bytes().to_vec()),
            _ => None,
        };

        Ok(PluginRequestSpec {
            method,
            url,
            headers,
            body,
        })
    }
}

impl netanvil_types::RequestGenerator for Lua54Generator {
    fn generate(&mut self, context: &netanvil_types::RequestContext) -> netanvil_types::RequestSpec {
        let plugin_ctx = PluginRequestContext::from(context);

        let ctx_table = self
            .ctx_to_table(&plugin_ctx)
            .expect("failed to create Lua context table");

        let generate_fn: LuaFunction = self
            .lua
            .globals()
            .get("generate")
            .expect("Lua script must define generate()");

        let result_table: LuaTable = generate_fn
            .call(ctx_table)
            .expect("Lua generate() call failed");

        let spec = self
            .table_to_spec(result_table)
            .expect("Lua generate() returned invalid table");

        spec.into_request_spec()
    }

    fn update_targets(&mut self, targets: Vec<String>) {
        if let Ok(update_fn) = self.lua.globals().get::<LuaFunction>("update_targets") {
            if let Ok(table) = self
                .lua
                .create_sequence_from(targets.iter().map(|s| s.as_str()))
            {
                let _ = update_fn.call::<()>(table);
            }
        }
    }
}

/// Parse a Lua hybrid configuration script and return a `GeneratorConfig`.
///
/// Same as `netanvil_plugin_luajit::config_from_lua` but uses Lua 5.4 interpreter.
pub fn config_from_lua(script: &str) -> Result<GeneratorConfig> {
    let lua = Lua::new();
    lua.load(script)
        .exec()
        .map_err(|e| PluginError::Lua(format!("load: {e}")))?;

    let configure_fn: LuaFunction = lua
        .globals()
        .get("configure")
        .map_err(|e| PluginError::Lua(format!("missing configure(): {e}")))?;

    let table: LuaTable = configure_fn
        .call(())
        .map_err(|e| PluginError::Lua(format!("configure() call: {e}")))?;

    let method: String = table.get("method").unwrap_or_else(|_| "GET".into());

    let url_patterns: Vec<WeightedPattern> = {
        let patterns_table: LuaTable = table
            .get("url_patterns")
            .map_err(|e| PluginError::Lua(format!("missing url_patterns: {e}")))?;
        let mut patterns = Vec::new();
        for entry in patterns_table.sequence_values::<LuaTable>() {
            let entry = entry.map_err(|e| PluginError::Lua(format!("url_patterns entry: {e}")))?;
            let pattern: String = entry
                .get("pattern")
                .map_err(|e| PluginError::Lua(format!("missing pattern: {e}")))?;
            let weight: f64 = entry.get("weight").unwrap_or(1.0);
            patterns.push(WeightedPattern { pattern, weight });
        }
        patterns
    };

    let headers: Vec<(String, String)> = match table.get::<LuaTable>("headers") {
        Ok(h) => {
            let mut headers = Vec::new();
            for pair in h.sequence_values::<LuaTable>().flatten() {
                let k: String = pair.get(1).unwrap_or_default();
                let v: String = pair.get(2).unwrap_or_default();
                headers.push((k, v));
            }
            headers
        }
        Err(_) => Vec::new(),
    };

    let body_template: Option<String> = table.get("body_template").ok();

    Ok(GeneratorConfig {
        url_patterns,
        method,
        headers,
        body_template,
    })
}
