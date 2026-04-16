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

use std::marker::PhantomData;

use rhai::{Dynamic, Engine, Map, Scope, AST};

use crate::error::{PluginError, Result};
use crate::types::{PluginHttpRequestSpec, PluginRequestContext, ResponseConfig};

/// A RequestGenerator backed by a Rhai script.
///
/// The Rhai script must define a function `generate(ctx)` that receives an
/// object map with `{request_id, core_id, is_sampled, session_id}` and
/// returns an object map `{method, url, headers, body}`.
pub struct RhaiGenerator<S: FromRhaiPlugin> {
    engine: Engine,
    ast: AST,
    scope: Scope<'static>,
    has_on_response: bool,
    response_cfg: ResponseConfig,
    _phantom: PhantomData<S>,
}

impl<S: FromRhaiPlugin> RhaiGenerator<S> {
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

        let has_on_response = ast.iter_functions().any(|f| f.name == "on_response");

        // Detect response_config() for what data the plugin needs.
        let response_cfg = if has_on_response {
            let has_config = ast.iter_functions().any(|f| f.name == "response_config");
            if has_config {
                let result: std::result::Result<Map, _> =
                    engine.call_fn(&mut scope, &ast, "response_config", ());
                if let Ok(map) = result {
                    ResponseConfig {
                        headers: map
                            .get("headers")
                            .and_then(|v| v.as_bool().ok())
                            .unwrap_or(false),
                        body: map
                            .get("body")
                            .and_then(|v| v.as_bool().ok())
                            .unwrap_or(false),
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

        Ok(Self {
            engine,
            ast,
            scope,
            has_on_response,
            response_cfg,
            _phantom: PhantomData,
        })
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
}

impl<S: FromRhaiPlugin> netanvil_types::RequestGenerator for RhaiGenerator<S> {
    type Spec = S;

    fn generate(&mut self, context: &netanvil_types::RequestContext) -> S {
        let plugin_ctx = PluginRequestContext::from(context);
        let ctx_map = self.ctx_to_map(&plugin_ctx);

        let result: Dynamic = self
            .engine
            .call_fn(&mut self.scope, &self.ast, "generate", (ctx_map,))
            .expect("Rhai generate() call failed");

        let result_map: Map = result.cast();
        S::from_rhai_map(&result_map).unwrap_or_else(|_| S::fallback())
    }

    fn on_response(&mut self, result: &netanvil_types::ExecutionResult) {
        if !self.has_on_response {
            return;
        }
        let mut map = Map::new();
        map.insert("request_id".into(), Dynamic::from(result.request_id as i64));
        map.insert(
            "status".into(),
            result
                .status
                .map(|s| Dynamic::from(s as i64))
                .unwrap_or(Dynamic::UNIT),
        );
        map.insert(
            "latency_ms".into(),
            Dynamic::from(result.timing.total.as_secs_f64() * 1000.0),
        );
        map.insert("bytes_sent".into(), Dynamic::from(result.bytes_sent as i64));
        map.insert(
            "response_size".into(),
            Dynamic::from(result.response_size as i64),
        );
        if let Some(ref err) = result.error {
            map.insert("error".into(), Dynamic::from(err.to_string()));
        }
        let _ = self
            .engine
            .call_fn::<()>(&mut self.scope, &self.ast, "on_response", (map,));
    }

    fn wants_responses(&self) -> bool {
        self.has_on_response
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

impl FromRhaiPlugin for netanvil_types::TcpRequestSpec {
    fn from_rhai_map(map: &Map) -> std::result::Result<Self, PluginError> {
        let payload = map
            .get("payload")
            .and_then(|v| v.clone().into_string().ok())
            .map(|s| s.into_bytes())
            .unwrap_or_default();

        Ok(netanvil_types::TcpRequestSpec {
            target: "0.0.0.0:0".parse().unwrap(),
            payload,
            framing: netanvil_types::TcpFraming::Raw,
            expect_response: true,
            response_max_bytes: 65536,
            mode: netanvil_types::TcpTestMode::Echo,
            request_size: 0,
            response_size: 0,
            latency_us: None,
            error_rate: None,
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
            latency_us: None,
            error_rate: None,
        }
    }
}

impl FromRhaiPlugin for netanvil_types::DnsRequestSpec {
    fn from_rhai_map(map: &Map) -> std::result::Result<Self, PluginError> {
        let query_name = map
            .get("query_name")
            .and_then(|v| v.clone().into_string().ok())
            .ok_or_else(|| PluginError::InvalidResponse("missing 'query_name'".into()))?;

        let query_type_str = map
            .get("query_type")
            .and_then(|v| v.clone().into_string().ok())
            .unwrap_or_else(|| "A".into());

        let recursion = map
            .get("recursion")
            .and_then(|v| v.as_bool().ok())
            .unwrap_or(true);

        let dnssec = map
            .get("dnssec")
            .and_then(|v| v.as_bool().ok())
            .unwrap_or(false);

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

impl FromRhaiPlugin for netanvil_types::RedisRequestSpec {
    fn from_rhai_map(map: &Map) -> std::result::Result<Self, PluginError> {
        let command = map
            .get("command")
            .and_then(|v| v.clone().into_string().ok())
            .ok_or_else(|| PluginError::InvalidResponse("missing 'command' in response".into()))?;

        let args = match map.get("args") {
            Some(v) => {
                if let Ok(arr) = v.clone().into_array() {
                    arr.into_iter()
                        .filter_map(|item| item.into_string().ok())
                        .collect()
                } else {
                    Vec::new()
                }
            }
            None => Vec::new(),
        };

        Ok(netanvil_types::RedisRequestSpec {
            target: "0.0.0.0:0".parse().unwrap(),
            command,
            args,
        })
    }

    fn fallback() -> Self {
        netanvil_types::RedisRequestSpec {
            target: "127.0.0.1:6379".parse().unwrap(),
            command: "PING".into(),
            args: vec![],
        }
    }
}

impl FromRhaiPlugin for netanvil_types::UdpRequestSpec {
    fn from_rhai_map(map: &Map) -> std::result::Result<Self, PluginError> {
        let payload = map
            .get("payload")
            .and_then(|v| v.clone().into_string().ok())
            .map(|s| s.into_bytes())
            .unwrap_or_default();

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
