//! Rhai plugin runtime.
//!
//! Rhai is a Rust-native scripting language designed for embedding. It has
//! zero unsafe code and a familiar syntax. Performance is lower than LuaJIT
//! but it has excellent Rust integration.
//!
//! # Instance-per-core
//!
//! Each `RhaiGenerator` owns its own `Engine` and `AST`. The Rhai Engine is
//! not the same as wasmtime's Engine — it includes the interpreter state.

use rhai::{Dynamic, Engine, Map, Scope, AST};

use crate::error::{PluginError, Result};
use crate::types::{PluginHttpRequestSpec, PluginRequestContext};

/// A RequestGenerator backed by a Rhai script.
///
/// The Rhai script must define a function `generate(ctx)` that receives an
/// object map with `{request_id, core_id, is_sampled, session_id}` and
/// returns an object map `{method, url, headers, body}`.
pub struct RhaiGenerator {
    engine: Engine,
    ast: AST,
    scope: Scope<'static>,
}

impl RhaiGenerator {
    /// Create a new RhaiGenerator from Rhai source code.
    pub fn new(script: &str, targets: &[String]) -> Result<Self> {
        let engine = Engine::new();

        let ast = engine
            .compile(script)
            .map_err(|e| PluginError::Rhai(format!("compile: {e}")))?;

        let mut scope = Scope::new();

        // Set targets as a global variable accessible from the script
        let targets_array: rhai::Array = targets.iter().map(|s| Dynamic::from(s.clone())).collect();
        scope.push("targets", targets_array);
        scope.push("counter", 0_i64);

        // Run the script once to define functions and initialize state
        engine
            .run_ast_with_scope(&mut scope, &ast)
            .map_err(|e| PluginError::Rhai(format!("init: {e}")))?;

        Ok(Self { engine, ast, scope })
    }

    fn ctx_to_map(&self, ctx: &PluginRequestContext) -> Map {
        let mut map = Map::new();
        map.insert("request_id".into(), Dynamic::from(ctx.request_id as i64));
        map.insert("core_id".into(), Dynamic::from(ctx.core_id as i64));
        map.insert("is_sampled".into(), Dynamic::from(ctx.is_sampled));
        match ctx.session_id {
            Some(id) => map.insert("session_id".into(), Dynamic::from(id as i64)),
            None => map.insert("session_id".into(), Dynamic::UNIT),
        };
        map
    }

    fn map_to_spec(&self, map: Map) -> Result<PluginHttpRequestSpec> {
        let method = map
            .get("method")
            .and_then(|v| v.clone().into_string().ok())
            .unwrap_or_else(|| "GET".into());

        let url = map
            .get("url")
            .and_then(|v| v.clone().into_string().ok())
            .ok_or_else(|| PluginError::InvalidResponse("missing 'url' in response".into()))?;

        let headers = match map.get("headers") {
            Some(v) => {
                if let Ok(arr) = v.clone().into_array() {
                    arr.into_iter()
                        .filter_map(|pair| {
                            let pair = pair.into_array().ok()?;
                            if pair.len() >= 2 {
                                let k = pair[0].clone().into_string().ok()?;
                                let v = pair[1].clone().into_string().ok()?;
                                Some((k, v))
                            } else {
                                None
                            }
                        })
                        .collect()
                } else {
                    Vec::new()
                }
            }
            None => Vec::new(),
        };

        let body = map
            .get("body")
            .and_then(|v| v.clone().into_string().ok())
            .map(|s| s.into_bytes());

        Ok(PluginHttpRequestSpec {
            method,
            url,
            headers,
            body,
        })
    }
}

impl netanvil_types::RequestGenerator for RhaiGenerator {
    type Spec = netanvil_types::HttpRequestSpec;

    fn generate(
        &mut self,
        context: &netanvil_types::RequestContext,
    ) -> netanvil_types::HttpRequestSpec {
        let plugin_ctx = PluginRequestContext::from(context);
        let ctx_map = self.ctx_to_map(&plugin_ctx);

        let result: Dynamic = self
            .engine
            .call_fn(&mut self.scope, &self.ast, "generate", (ctx_map,))
            .expect("Rhai generate() call failed");

        let result_map: Map = result.cast();
        let spec = self
            .map_to_spec(result_map)
            .expect("Rhai generate() returned invalid map");

        spec.into_http_request_spec()
    }

    fn update_targets(&mut self, targets: Vec<String>) {
        let targets_array: rhai::Array = targets.iter().map(|s| Dynamic::from(s.clone())).collect();
        self.scope.set_value("targets", targets_array);
    }
}

// ---------------------------------------------------------------------------
// Protocol-generic Rhai plugin output conversion.
// ---------------------------------------------------------------------------

/// Construct a protocol-specific spec from a Rhai dynamic map.
///
/// Each protocol implements this for its spec type. The Rhai generator calls
/// `S::from_rhai_map()` instead of hardcoding the HTTP field extraction.
pub trait FromRhaiPlugin: netanvil_types::ProtocolSpec + Sized {
    fn from_rhai_map(map: &Map) -> std::result::Result<Self, PluginError>;
    fn fallback() -> Self;
}

impl FromRhaiPlugin for netanvil_types::HttpRequestSpec {
    fn from_rhai_map(map: &Map) -> std::result::Result<Self, PluginError> {
        let method = map
            .get("method")
            .and_then(|v| v.clone().into_string().ok())
            .unwrap_or_else(|| "GET".into());

        let url = map
            .get("url")
            .and_then(|v| v.clone().into_string().ok())
            .ok_or_else(|| PluginError::InvalidResponse("missing 'url' in response".into()))?;

        let headers = match map.get("headers") {
            Some(v) => {
                if let Ok(arr) = v.clone().into_array() {
                    arr.into_iter()
                        .filter_map(|pair| {
                            let pair = pair.into_array().ok()?;
                            if pair.len() >= 2 {
                                let k = pair[0].clone().into_string().ok()?;
                                let v = pair[1].clone().into_string().ok()?;
                                Some((k, v))
                            } else {
                                None
                            }
                        })
                        .collect()
                } else {
                    Vec::new()
                }
            }
            None => Vec::new(),
        };

        let body = map
            .get("body")
            .and_then(|v| v.clone().into_string().ok())
            .map(|s| s.into_bytes());

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
