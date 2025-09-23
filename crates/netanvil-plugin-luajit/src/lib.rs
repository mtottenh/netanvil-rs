//! LuaJIT plugin runtime for netanvil-rs.
//!
//! Provides `LuaJitGenerator` — a `RequestGenerator` backed by LuaJIT via mlua.
//! LuaJIT's tracing JIT compiler makes this the fastest scripting option (~2.1μs/call).
//!
//! Also provides `config_from_lua` for the hybrid approach (Lua config → native execution).

use std::marker::PhantomData;

use mlua::prelude::*;

use netanvil_plugin::error::{PluginError, Result};
use netanvil_plugin::hybrid::{GeneratorConfig, WeightedPattern};
use netanvil_plugin::types::{PluginHttpRequestSpec, PluginRequestContext, ResponseConfig};

/// A RequestGenerator backed by LuaJIT.
///
/// The Lua script must define a global function `generate(ctx)` that receives
/// a table with fields `{request_id, core_id, is_sampled, session_id}` and
/// returns a table `{method, url, headers, body}`.
///
/// Optionally:
/// - `init(targets)` — called once with the target URL list
/// - `update_targets(targets)` — called to update targets mid-test
pub struct LuaJitGenerator<S: FromLuaPlugin> {
    lua: Lua,
    /// Persistent context table — reused every call to avoid GC allocation.
    ctx_table: LuaTable,
    /// Persistent result table — reused every on_response call to avoid GC allocation.
    result_table: Option<LuaTable>,
    /// Persistent headers sub-table inside result_table.
    headers_table: Option<LuaTable>,
    /// Whether the script defines an `on_response(result)` function.
    has_on_response: bool,
    /// What response data the plugin needs.
    response_cfg: ResponseConfig,
    _phantom: PhantomData<S>,
}

impl<S: FromLuaPlugin> LuaJitGenerator<S> {
    /// Create a new LuaJitGenerator from Lua source code.
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

        // Pre-allocate the context table once — mutated per call instead of reallocated.
        let ctx_table = lua
            .create_table()
            .map_err(|e| PluginError::Lua(format!("create ctx table: {e}")))?;
        ctx_table
            .set("request_id", 0u64)
            .map_err(|e| PluginError::Lua(format!("init ctx table: {e}")))?;
        ctx_table
            .set("core_id", 0usize)
            .map_err(|e| PluginError::Lua(format!("init ctx table: {e}")))?;
        ctx_table
            .set("is_sampled", false)
            .map_err(|e| PluginError::Lua(format!("init ctx table: {e}")))?;
        ctx_table
            .set("session_id", LuaNil)
            .map_err(|e| PluginError::Lua(format!("init ctx table: {e}")))?;

        let has_on_response = lua.globals().get::<LuaFunction>("on_response").is_ok();

        // Detect response_config() for what data the plugin needs.
        let response_cfg = if has_on_response {
            if let Ok(config_fn) = lua.globals().get::<LuaFunction>("response_config") {
                if let Ok(table) = config_fn.call::<LuaTable>(()) {
                    ResponseConfig {
                        headers: table.get("headers").unwrap_or(false),
                        body: table.get("body").unwrap_or(false),
                    }
                } else {
                    ResponseConfig::on_response_default()
                }
            } else {
                ResponseConfig::on_response_default()
            }
        } else {
            ResponseConfig::default()
        };

        // Pre-allocate persistent result table (like ctx_table) — reused per on_response call.
        let (result_table, headers_table) = if has_on_response {
            let rt = lua
                .create_table()
                .map_err(|e| PluginError::Lua(format!("create result table: {e}")))?;
            rt.set("request_id", 0u64)
                .map_err(|e| PluginError::Lua(format!("init result table: {e}")))?;
            rt.set("status", LuaNil)
                .map_err(|e| PluginError::Lua(format!("init result table: {e}")))?;
            rt.set("latency_ms", 0.0)
                .map_err(|e| PluginError::Lua(format!("init result table: {e}")))?;
            rt.set("bytes_sent", 0u64)
                .map_err(|e| PluginError::Lua(format!("init result table: {e}")))?;
            rt.set("response_size", 0u64)
                .map_err(|e| PluginError::Lua(format!("init result table: {e}")))?;

            let ht = if response_cfg.headers {
                let ht = lua
                    .create_table()
                    .map_err(|e| PluginError::Lua(format!("create headers table: {e}")))?;
                rt.set("headers", ht.clone())
                    .map_err(|e| PluginError::Lua(format!("set headers table: {e}")))?;
                Some(ht)
            } else {
                None
            };

            (Some(rt), ht)
        } else {
            (None, None)
        };

        Ok(Self {
            lua,
            ctx_table,
            result_table,
            headers_table,
            has_on_response,
            response_cfg,
            _phantom: PhantomData,
        })
    }

    /// Update the persistent result table with new values (minimal allocation).
    fn update_result_table(&self, result: &netanvil_types::ExecutionResult) {
        let Some(ref rt) = self.result_table else {
            return;
        };

        // Fixed fields: mutate existing table values (no allocation).
        rt.set("request_id", result.request_id)
            .expect("result table set failed");
        rt.set("status", result.status)
            .expect("result table set failed");
        rt.set("latency_ms", result.timing.total.as_secs_f64() * 1000.0)
            .expect("result table set failed");
        rt.set("bytes_sent", result.bytes_sent)
            .expect("result table set failed");
        rt.set("response_size", result.response_size)
            .expect("result table set failed");

        // Error: string allocation only when present (unavoidable).
        match &result.error {
            Some(err) => rt.set("error", err.to_string()).expect("set error"),
            None => rt.set("error", LuaNil).expect("clear error"),
        }

        // Headers: reuse persistent sub-table, clear and repopulate.
        if self.response_cfg.headers {
            if let Some(ref ht) = self.headers_table {
                // Clear existing entries
                let keys: Vec<String> = ht
                    .pairs::<String, LuaValue>()
                    .filter_map(|r| r.ok().map(|(k, _)| k))
                    .collect();
                for key in keys {
                    let _ = ht.set(key, LuaNil);
                }
                // Populate from response
                if let Some(ref headers) = result.response_headers {
                    for (k, v) in headers {
                        let _ = ht.set(k.as_str(), v.as_str());
                    }
                }
            }
        }

        // Body: string allocation when present (unavoidable for Lua strings).
        if self.response_cfg.body {
            if let Some(ref body) = result.response_body {
                if let Ok(s) = self.lua.create_string(body.as_ref()) {
                    let _ = rt.set("body", s);
                }
            } else {
                let _ = rt.set("body", LuaNil);
            }
        }
    }

    /// Update the persistent context table with new values (no allocation).
    fn update_ctx_table(&self, ctx: &PluginRequestContext) {
        self.ctx_table
            .set("request_id", ctx.request_id)
            .expect("ctx table set failed");
        self.ctx_table
            .set("core_id", ctx.core_id)
            .expect("ctx table set failed");
        self.ctx_table
            .set("is_sampled", ctx.is_sampled)
            .expect("ctx table set failed");
        match ctx.session_id {
            Some(id) => self.ctx_table.set("session_id", id),
            None => self.ctx_table.set("session_id", LuaNil),
        }
        .expect("ctx table set failed");
    }
}

impl<S: FromLuaPlugin> netanvil_types::RequestGenerator for LuaJitGenerator<S> {
    type Spec = S;

    fn generate(&mut self, context: &netanvil_types::RequestContext) -> S {
        let plugin_ctx = PluginRequestContext::from(context);

        // Reuse persistent table — avoids GC allocation per call.
        self.update_ctx_table(&plugin_ctx);

        let generate_fn: LuaFunction = self
            .lua
            .globals()
            .get("generate")
            .expect("Lua script must define generate()");

        let result_table: LuaTable = generate_fn
            .call(self.ctx_table.clone())
            .expect("Lua generate() call failed");

        S::from_lua_table(&result_table).unwrap_or_else(|_| S::fallback())
    }

    fn on_response(&mut self, result: &netanvil_types::ExecutionResult) {
        if !self.has_on_response {
            return;
        }
        // Update persistent result table (mutate, don't allocate new table).
        self.update_result_table(result);
        if let Ok(on_resp) = self.lua.globals().get::<LuaFunction>("on_response") {
            if let Some(ref rt) = self.result_table {
                let _ = on_resp.call::<()>(rt.clone());
            }
        }
    }

    fn wants_responses(&self) -> bool {
        self.has_on_response
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

// ---------------------------------------------------------------------------
// Protocol-generic Lua plugin output conversion.
// ---------------------------------------------------------------------------

/// Construct a protocol-specific spec from a Lua table.
///
/// Each protocol implements this for its spec type. The LuaJIT generator calls
/// `S::from_lua_table()` instead of hardcoding the HTTP field extraction.
pub trait FromLuaPlugin: netanvil_types::ProtocolSpec + Sized {
    fn from_lua_table(table: &LuaTable) -> std::result::Result<Self, PluginError>;
    fn fallback() -> Self;
}

impl FromLuaPlugin for netanvil_types::HttpRequestSpec {
    fn from_lua_table(table: &LuaTable) -> std::result::Result<Self, PluginError> {
        let method: String = table
            .get("method")
            .map_err(|e| PluginError::InvalidResponse(format!("method: {e}")))?;
        let url: String = table
            .get("url")
            .map_err(|e| PluginError::InvalidResponse(format!("url: {e}")))?;

        let headers: Vec<(String, String)> = match table.get::<LuaTable>("headers") {
            Ok(h) => {
                let mut headers = Vec::new();
                for pair in h.sequence_values::<LuaTable>().flatten() {
                    if let (Ok(k), Ok(v)) = (pair.get::<String>(1), pair.get::<String>(2)) {
                        headers.push((k, v));
                    }
                }
                headers
            }
            Err(_) => Vec::new(),
        };

        let body: Option<Vec<u8>> = match table.get::<LuaValue>("body") {
            Ok(LuaValue::String(s)) => Some(s.as_bytes().to_vec()),
            _ => None,
        };

        let plugin_spec = PluginHttpRequestSpec {
            method,
            url,
            headers,
            body,
        };
        Ok(plugin_spec.into_http_request_spec())
    }

    fn fallback() -> Self {
        netanvil_types::HttpRequestSpec {
            method: http::Method::GET,
            url: "http://error.invalid".into(),
            headers: vec![],
            body: None,
        }
    }
}

impl FromLuaPlugin for netanvil_types::TcpRequestSpec {
    fn from_lua_table(table: &LuaTable) -> std::result::Result<Self, PluginError> {
        // Plugin returns {payload = "..."} — target/framing/mode come from config.
        let payload: Vec<u8> = if let Ok(s) = table.get::<String>("payload") {
            s.into_bytes()
        } else if let Ok(LuaValue::String(s)) = table.get::<LuaValue>("payload") {
            s.as_bytes().to_vec()
        } else {
            vec![]
        };

        Ok(netanvil_types::TcpRequestSpec {
            target: "0.0.0.0:0".parse().unwrap(),
            payload,
            framing: netanvil_types::TcpFraming::Raw,
            expect_response: true,
            response_max_bytes: 65536,
            mode: netanvil_types::TcpTestMode::Echo,
            request_size: 0,
            response_size: 0,
        })
    }

    fn fallback() -> Self {
        netanvil_types::TcpRequestSpec {
            target: "127.0.0.1:0".parse().unwrap(),
            payload: vec![],
            framing: netanvil_types::TcpFraming::Raw,
            expect_response: true,
            response_max_bytes: 65536,
            mode: netanvil_types::TcpTestMode::Echo,
            request_size: 0,
            response_size: 0,
        }
    }
}

impl FromLuaPlugin for netanvil_types::DnsRequestSpec {
    fn from_lua_table(table: &LuaTable) -> std::result::Result<Self, PluginError> {
        let query_name: String = table
            .get("query_name")
            .map_err(|e| PluginError::InvalidResponse(format!("query_name: {e}")))?;
        let query_type_str: String = table.get("query_type").unwrap_or_else(|_| "A".into());
        let recursion: bool = table.get("recursion").unwrap_or(true);
        let dnssec: bool = table.get("dnssec").unwrap_or(false);

        let query_type = netanvil_types::DnsQueryType::from_str_name(&query_type_str)
            .unwrap_or(netanvil_types::DnsQueryType::A);

        Ok(netanvil_types::DnsRequestSpec {
            server: "0.0.0.0:0".parse().unwrap(),
            query_name,
            query_type,
            recursion,
            dnssec,
            txid: 0,
        })
    }

    fn fallback() -> Self {
        netanvil_types::DnsRequestSpec {
            server: "127.0.0.1:53".parse().unwrap(),
            query_name: "error.invalid".into(),
            query_type: netanvil_types::DnsQueryType::A,
            recursion: true,
            dnssec: false,
            txid: 0,
        }
    }
}

impl FromLuaPlugin for netanvil_types::UdpRequestSpec {
    fn from_lua_table(table: &LuaTable) -> std::result::Result<Self, PluginError> {
        let payload: Vec<u8> = if let Ok(s) = table.get::<String>("payload") {
            s.into_bytes()
        } else if let Ok(LuaValue::String(s)) = table.get::<LuaValue>("payload") {
            s.as_bytes().to_vec()
        } else {
            vec![]
        };

        Ok(netanvil_types::UdpRequestSpec {
            target: "0.0.0.0:0".parse().unwrap(),
            payload,
            expect_response: true,
            response_max_bytes: 65536,
        })
    }

    fn fallback() -> Self {
        netanvil_types::UdpRequestSpec {
            target: "127.0.0.1:0".parse().unwrap(),
            payload: vec![],
            expect_response: true,
            response_max_bytes: 65536,
        }
    }
}

/// Parse a Lua hybrid configuration script and return a `GeneratorConfig`.
///
/// The script must define a `configure()` function returning a table with:
/// `{method, url_patterns: [{pattern, weight}], headers: [[k,v]], body_template}`.
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
