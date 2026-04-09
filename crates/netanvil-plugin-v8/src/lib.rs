//! V8 JavaScript plugin runtime for netanvil-rs.
//!
//! Provides `V8Generator` — a `RequestGenerator` backed by V8 via rusty_v8.
//! Requires the `v8` feature flag on consumer crates due to V8's large binary
//! size and compile time.
//!
//! ## Isolate lifecycle
//!
//! V8 isolates are single-threaded, matching the `!Send` thread-per-core model.
//! A **thread-local isolate** is created once per thread and shared by all
//! generators on that thread via separate `v8::Context`s. This avoids V8
//! isolate dispose-then-create lifecycle issues and retains full JIT.
//!
//! ## Script interface
//!
//! The JS script must define a global function `generate(ctx)` that receives
//! an object with fields `{request_id, core_id, is_sampled, session_id}` and
//! returns an object `{method, url, headers, body}`.
//!
//! Optionally:
//! - `init(targets)` — called once at startup with the target URL array
//! - `on_response(result)` — called with each completed request result
//! - `response_config()` — returns `{headers, body}` flags for on_response data
//! - `update_targets(targets)` — called to update targets mid-test

use std::cell::RefCell;
use std::marker::PhantomData;
use std::sync::Once;

use netanvil_plugin::error::{PluginError, Result};
use netanvil_plugin::types::{PluginHttpRequestSpec, PluginRequestContext, ResponseConfig};

// ---------------------------------------------------------------------------
// V8 platform initialization (once per process) + thread-local isolate.
// ---------------------------------------------------------------------------

static V8_INIT: Once = Once::new();

fn ensure_v8_initialized() {
    V8_INIT.call_once(|| {
        let platform = v8::new_default_platform(0, false).make_shared();
        v8::V8::initialize_platform(platform);
        v8::V8::initialize();
    });
}

thread_local! {
    /// One V8 isolate per thread. Created on first use, lives until thread exit.
    /// All `V8Generator`s on this thread share the isolate via separate Contexts.
    static V8_ISOLATE: RefCell<Option<v8::OwnedIsolate>> = RefCell::new(None);
}

/// Run a closure with the thread-local V8 isolate, creating it on first use.
fn with_isolate<R>(f: impl FnOnce(&mut v8::OwnedIsolate) -> R) -> R {
    ensure_v8_initialized();
    V8_ISOLATE.with(|cell| {
        let mut opt = cell.borrow_mut();
        if opt.is_none() {
            *opt = Some(v8::Isolate::new(v8::CreateParams::default()));
        }
        f(opt.as_mut().unwrap())
    })
}

// ---------------------------------------------------------------------------
// V8 helpers — all use the v147 PinScope / scope!() API.
// ---------------------------------------------------------------------------

fn v8_string<'s>(scope: &mut v8::PinScope<'s, '_>, s: &str) -> v8::Local<'s, v8::String> {
    v8::String::new(scope, s).expect("v8::String::new failed")
}

fn get_function(
    scope: &mut v8::PinScope<'_, '_>,
    global: &v8::Object,
    name: &str,
) -> Option<v8::Global<v8::Function>> {
    let key = v8_string(scope, name);
    let val = global.get(scope, key.into())?;
    let func = v8::Local::<v8::Function>::try_from(val).ok()?;
    Some(v8::Global::new(scope, func))
}

fn get_string_property(
    scope: &mut v8::PinScope<'_, '_>,
    obj: &v8::Object,
    key: &str,
) -> Option<String> {
    let k = v8_string(scope, key);
    let val = obj.get(scope, k.into())?;
    if val.is_undefined() || val.is_null() {
        return None;
    }
    let s = val.to_string(scope)?;
    Some(s.to_rust_string_lossy(scope))
}

fn get_bool_property(
    scope: &mut v8::PinScope<'_, '_>,
    obj: &v8::Object,
    key: &str,
) -> Option<bool> {
    let k = v8_string(scope, key);
    let val = obj.get(scope, k.into())?;
    if val.is_undefined() || val.is_null() {
        return None;
    }
    Some(val.boolean_value(scope))
}

fn set_string_property(scope: &mut v8::PinScope<'_, '_>, obj: &v8::Object, key: &str, value: &str) {
    let k = v8_string(scope, key);
    let v = v8_string(scope, value);
    obj.set(scope, k.into(), v.into());
}

fn set_number_property(scope: &mut v8::PinScope<'_, '_>, obj: &v8::Object, key: &str, value: f64) {
    let k = v8_string(scope, key);
    let v = v8::Number::new(scope, value);
    obj.set(scope, k.into(), v.into());
}

fn set_bool_property(scope: &mut v8::PinScope<'_, '_>, obj: &v8::Object, key: &str, value: bool) {
    let k = v8_string(scope, key);
    let v = v8::Boolean::new(scope, value);
    obj.set(scope, k.into(), v.into());
}

fn set_null_property(scope: &mut v8::PinScope<'_, '_>, obj: &v8::Object, key: &str) {
    let k = v8_string(scope, key);
    let v = v8::null(scope);
    obj.set(scope, k.into(), v.into());
}

// ---------------------------------------------------------------------------
// V8Generator.
// ---------------------------------------------------------------------------

/// A RequestGenerator backed by V8 (rusty_v8).
///
/// Uses a **thread-local isolate** shared by all generators on the same thread.
/// Each generator gets its own `v8::Context` for script isolation. This matches
/// the thread-per-core model (one isolate per I/O worker, never shared across
/// cores).
///
/// `Global` handles are safe to auto-drop because the thread-local isolate
/// outlives the generator.
pub struct V8Generator<S: FromV8Plugin> {
    /// V8 context — isolates this generator's script state.
    context: v8::Global<v8::Context>,
    /// Persistent context object — reused every call to avoid GC allocation.
    ctx_object: v8::Global<v8::Object>,
    /// Cached `generate` function handle.
    generate_fn: v8::Global<v8::Function>,
    /// Cached `on_response` function handle (if script defines it).
    on_response_fn: Option<v8::Global<v8::Function>>,
    /// Cached `update_targets` function handle (if script defines it).
    update_targets_fn: Option<v8::Global<v8::Function>>,
    /// Persistent result object — reused every on_response call.
    result_object: Option<v8::Global<v8::Object>>,
    /// Persistent headers object inside result_object.
    headers_object: Option<v8::Global<v8::Object>>,
    /// Whether the script defines an `on_response(result)` function.
    has_on_response: bool,
    /// What response data the plugin needs.
    response_cfg: ResponseConfig,
    _phantom: PhantomData<S>,
}

impl<S: FromV8Plugin> V8Generator<S> {
    /// Create a new V8Generator from JavaScript source code.
    pub fn new(script: &str, targets: &[String]) -> Result<Self> {
        with_isolate(|isolate| {
            v8::scope!(let scope, isolate);

            let context = v8::Context::new(scope, Default::default());
            let scope = &mut v8::ContextScope::new(scope, context);

            // Compile and run the script.
            let source = v8_string(scope, script);
            let script_obj = v8::Script::compile(scope, source, None)
                .ok_or_else(|| PluginError::V8("script compilation failed".into()))?;
            script_obj
                .run(scope)
                .ok_or_else(|| PluginError::V8("script execution failed".into()))?;

            let global = context.global(scope);

            // Extract the required generate() function.
            let generate_fn = get_function(scope, &*global, "generate")
                .ok_or_else(|| PluginError::V8("script must define generate()".into()))?;

            // Optional functions.
            let init_fn = get_function(scope, &*global, "init");
            let on_response_fn = get_function(scope, &*global, "on_response");
            let update_targets_fn = get_function(scope, &*global, "update_targets");
            let response_config_fn = get_function(scope, &*global, "response_config");

            let has_on_response = on_response_fn.is_some();

            // Call init(targets) if defined.
            if let Some(ref init_fn) = init_fn {
                let targets_array = v8::Array::new(scope, targets.len() as i32);
                for (i, target) in targets.iter().enumerate() {
                    let val = v8_string(scope, target);
                    targets_array.set_index(scope, i as u32, val.into());
                }
                let init_local = init_fn.open(scope);
                let undefined = v8::undefined(&**scope);
                init_local.call(scope, undefined.into(), &[targets_array.into()]);
            }

            // Detect response_config().
            let response_cfg = if has_on_response {
                if let Some(ref config_fn) = response_config_fn {
                    let config_local = config_fn.open(scope);
                    let undefined = v8::undefined(&**scope);
                    if let Some(result) = config_local.call(scope, undefined.into(), &[]) {
                        if let Ok(obj) = v8::Local::<v8::Object>::try_from(result) {
                            ResponseConfig {
                                headers: get_bool_property(scope, &*obj, "headers")
                                    .unwrap_or(false),
                                body: get_bool_property(scope, &*obj, "body").unwrap_or(false),
                            }
                        } else {
                            ResponseConfig::on_response_default()
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

            // Pre-allocate persistent context object.
            let ctx_obj = v8::Object::new(scope);
            set_number_property(scope, &*ctx_obj, "request_id", 0.0);
            set_number_property(scope, &*ctx_obj, "core_id", 0.0);
            set_bool_property(scope, &*ctx_obj, "is_sampled", false);
            set_null_property(scope, &*ctx_obj, "session_id");
            let ctx_object = v8::Global::new(scope, ctx_obj);

            // Pre-allocate persistent result object (for on_response).
            let (result_object, headers_object) = if has_on_response {
                let rt = v8::Object::new(scope);
                set_number_property(scope, &*rt, "request_id", 0.0);
                set_null_property(scope, &*rt, "status");
                set_number_property(scope, &*rt, "latency_ms", 0.0);
                set_number_property(scope, &*rt, "bytes_sent", 0.0);
                set_number_property(scope, &*rt, "response_size", 0.0);
                set_null_property(scope, &*rt, "error");

                let ht = if response_cfg.headers {
                    let ht = v8::Object::new(scope);
                    let k = v8_string(scope, "headers");
                    rt.set(scope, k.into(), ht.into());
                    Some(v8::Global::new(scope, ht))
                } else {
                    None
                };

                (Some(v8::Global::new(scope, rt)), ht)
            } else {
                (None, None)
            };

            let context = v8::Global::new(scope, context);

            Ok(Self {
                context,
                ctx_object,
                generate_fn,
                on_response_fn,
                update_targets_fn,
                result_object,
                headers_object,
                has_on_response,
                response_cfg,
                _phantom: PhantomData,
            })
        })
    }
}

impl<S: FromV8Plugin> netanvil_types::RequestGenerator for V8Generator<S> {
    type Spec = S;

    fn generate(&mut self, context: &netanvil_types::RequestContext) -> S {
        let plugin_ctx = PluginRequestContext::from(context);

        with_isolate(|isolate| {
            v8::scope!(let handle_scope, isolate);
            let v8_context = v8::Local::new(handle_scope, &self.context);
            let scope = &mut v8::ContextScope::new(handle_scope, v8_context);

            // Update persistent context object in-place (avoids GC allocation).
            let ctx_obj = self.ctx_object.open(scope);
            set_number_property(scope, ctx_obj, "request_id", plugin_ctx.request_id as f64);
            set_number_property(scope, ctx_obj, "core_id", plugin_ctx.core_id as f64);
            set_bool_property(scope, ctx_obj, "is_sampled", plugin_ctx.is_sampled);
            match plugin_ctx.session_id {
                Some(id) => set_number_property(scope, ctx_obj, "session_id", id as f64),
                None => set_null_property(scope, ctx_obj, "session_id"),
            }

            // Call generate(ctx).
            let generate_fn = self.generate_fn.open(scope);
            let undefined = v8::undefined(&**scope);
            let ctx_local = v8::Local::new(scope, &self.ctx_object);
            let result = generate_fn.call(scope, undefined.into(), &[ctx_local.into()]);

            match result {
                Some(val) => {
                    if let Ok(obj) = v8::Local::<v8::Object>::try_from(val) {
                        S::from_v8_object(scope, &*obj).unwrap_or_else(|_| S::fallback())
                    } else {
                        S::fallback()
                    }
                }
                None => S::fallback(), // JS exception
            }
        })
    }

    fn on_response(&mut self, result: &netanvil_types::ExecutionResult) {
        if !self.has_on_response {
            return;
        }

        with_isolate(|isolate| {
            v8::scope!(let handle_scope, isolate);
            let v8_context = v8::Local::new(handle_scope, &self.context);
            let scope = &mut v8::ContextScope::new(handle_scope, v8_context);

            // Update persistent result object in-place.
            if let Some(ref result_global) = self.result_object {
                let rt = result_global.open(scope);

                set_number_property(scope, rt, "request_id", result.request_id as f64);
                match result.status {
                    Some(s) => set_number_property(scope, rt, "status", s as f64),
                    None => set_null_property(scope, rt, "status"),
                }
                set_number_property(
                    scope,
                    rt,
                    "latency_ms",
                    result.timing.total.as_secs_f64() * 1000.0,
                );
                set_number_property(scope, rt, "bytes_sent", result.bytes_sent as f64);
                set_number_property(scope, rt, "response_size", result.response_size as f64);

                match &result.error {
                    Some(err) => set_string_property(scope, rt, "error", &err.to_string()),
                    None => set_null_property(scope, rt, "error"),
                }

                if self.response_cfg.headers {
                    if let Some(ref headers_global) = self.headers_object {
                        let ht = headers_global.open(scope);
                        if let Some(names) = ht.get_own_property_names(scope, Default::default()) {
                            for i in 0..names.length() {
                                if let Some(key) = names.get_index(scope, i) {
                                    ht.delete(scope, key);
                                }
                            }
                        }
                        if let Some(ref headers) = result.response_headers {
                            for (k, v) in headers {
                                set_string_property(scope, ht, k, v);
                            }
                        }
                    }
                }

                if self.response_cfg.body {
                    if let Some(ref body) = result.response_body {
                        if let Ok(s) = std::str::from_utf8(body) {
                            set_string_property(scope, rt, "body", s);
                        } else {
                            set_null_property(scope, rt, "body");
                        }
                    } else {
                        set_null_property(scope, rt, "body");
                    }
                }
            }

            // Call on_response(result).
            if let Some(ref on_response_fn) = self.on_response_fn {
                if let Some(ref result_global) = self.result_object {
                    let on_resp = on_response_fn.open(scope);
                    let rt = v8::Local::new(scope, result_global);
                    let undefined = v8::undefined(&**scope);
                    let _ = on_resp.call(scope, undefined.into(), &[rt.into()]);
                }
            }
        });
    }

    fn wants_responses(&self) -> bool {
        self.has_on_response
    }

    fn update_targets(&mut self, targets: Vec<String>) {
        let Some(ref update_fn) = self.update_targets_fn else {
            return;
        };

        with_isolate(|isolate| {
            v8::scope!(let handle_scope, isolate);
            let v8_context = v8::Local::new(handle_scope, &self.context);
            let scope = &mut v8::ContextScope::new(handle_scope, v8_context);

            let targets_array = v8::Array::new(scope, targets.len() as i32);
            for (i, target) in targets.iter().enumerate() {
                let val = v8_string(scope, target);
                targets_array.set_index(scope, i as u32, val.into());
            }
            let update_local = update_fn.open(scope);
            let undefined = v8::undefined(&**scope);
            let _ = update_local.call(scope, undefined.into(), &[targets_array.into()]);
        });
    }
}

// ---------------------------------------------------------------------------
// Protocol-generic V8 plugin output conversion.
// ---------------------------------------------------------------------------

/// Construct a protocol-specific spec from a V8 object.
///
/// Each protocol implements this for its spec type. The V8 generator calls
/// `S::from_v8_object()` instead of hardcoding the HTTP field extraction.
pub trait FromV8Plugin: netanvil_types::ProtocolSpec + Sized {
    fn from_v8_object(
        scope: &mut v8::PinScope<'_, '_>,
        obj: &v8::Object,
    ) -> std::result::Result<Self, PluginError>;
    fn fallback() -> Self;
}

impl FromV8Plugin for netanvil_types::HttpRequestSpec {
    fn from_v8_object(
        scope: &mut v8::PinScope<'_, '_>,
        obj: &v8::Object,
    ) -> std::result::Result<Self, PluginError> {
        let method = get_string_property(scope, obj, "method")
            .ok_or_else(|| PluginError::InvalidResponse("method missing".into()))?;
        let url = get_string_property(scope, obj, "url")
            .ok_or_else(|| PluginError::InvalidResponse("url missing".into()))?;

        let headers: Vec<(String, String)> = {
            let k = v8_string(scope, "headers");
            let mut headers = Vec::new();
            if let Some(val) = obj.get(scope, k.into()) {
                if let Ok(arr) = v8::Local::<v8::Array>::try_from(val) {
                    for i in 0..arr.length() {
                        if let Some(pair) = arr.get_index(scope, i) {
                            if let Ok(pair_arr) = v8::Local::<v8::Array>::try_from(pair) {
                                let hk = pair_arr
                                    .get_index(scope, 0)
                                    .and_then(|v| v.to_string(scope))
                                    .map(|s| s.to_rust_string_lossy(scope));
                                let hv = pair_arr
                                    .get_index(scope, 1)
                                    .and_then(|v| v.to_string(scope))
                                    .map(|s| s.to_rust_string_lossy(scope));
                                if let (Some(hk), Some(hv)) = (hk, hv) {
                                    headers.push((hk, hv));
                                }
                            }
                        }
                    }
                }
            }
            headers
        };

        let body: Option<Vec<u8>> = {
            let k = v8_string(scope, "body");
            obj.get(scope, k.into()).and_then(|val| {
                if val.is_undefined() || val.is_null() {
                    None
                } else {
                    val.to_string(scope)
                        .map(|s| s.to_rust_string_lossy(scope).into_bytes())
                }
            })
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

impl FromV8Plugin for netanvil_types::TcpRequestSpec {
    fn from_v8_object(
        scope: &mut v8::PinScope<'_, '_>,
        obj: &v8::Object,
    ) -> std::result::Result<Self, PluginError> {
        let payload: Vec<u8> = get_string_property(scope, obj, "payload")
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

impl FromV8Plugin for netanvil_types::UdpRequestSpec {
    fn from_v8_object(
        scope: &mut v8::PinScope<'_, '_>,
        obj: &v8::Object,
    ) -> std::result::Result<Self, PluginError> {
        let payload: Vec<u8> = get_string_property(scope, obj, "payload")
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

impl FromV8Plugin for netanvil_types::DnsRequestSpec {
    fn from_v8_object(
        scope: &mut v8::PinScope<'_, '_>,
        obj: &v8::Object,
    ) -> std::result::Result<Self, PluginError> {
        let query_name = get_string_property(scope, obj, "query_name")
            .ok_or_else(|| PluginError::InvalidResponse("query_name missing".into()))?;
        let query_type_str =
            get_string_property(scope, obj, "query_type").unwrap_or_else(|| "A".into());
        let recursion = get_bool_property(scope, obj, "recursion").unwrap_or(true);
        let dnssec = get_bool_property(scope, obj, "dnssec").unwrap_or(false);

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

#[cfg(test)]
mod tests {
    use super::*;
    use netanvil_types::RequestGenerator;

    #[test]
    fn basic_generate() {
        let script = r#"
            var targets = [];
            var counter = 0;
            function init(t) { targets = t; }
            function generate(ctx) {
                var idx = counter % targets.length;
                var url = targets[idx] + "?seq=" + counter + "&core=" + ctx.core_id;
                counter++;
                return {
                    method: "GET",
                    url: url,
                    headers: [["X-Request-ID", String(ctx.request_id)]],
                    body: null
                };
            }
        "#;
        let targets = vec!["http://localhost:8080".to_string()];
        let mut gen =
            V8Generator::<netanvil_types::HttpRequestSpec>::new(script, &targets).unwrap();
        let now = std::time::Instant::now();
        let ctx = netanvil_types::RequestContext {
            request_id: 42,
            core_id: 1,
            is_sampled: false,
            intended_time: now,
            actual_time: now,
            sent_time: now,
            dispatch_time: now,
            session_id: None,
        };
        let spec = gen.generate(&ctx);
        assert_eq!(spec.method, http::Method::GET);
        assert!(spec.url.contains("localhost:8080"));
        assert!(spec.url.contains("seq=0"));
        assert!(spec.url.contains("core=1"));
    }

    #[test]
    fn generate_with_body() {
        let script = r#"
            function generate(ctx) {
                return {
                    method: "POST",
                    url: "http://example.com/api",
                    headers: [["Content-Type", "application/json"]],
                    body: '{"id":' + ctx.request_id + '}'
                };
            }
        "#;
        let mut gen = V8Generator::<netanvil_types::HttpRequestSpec>::new(script, &[]).unwrap();
        let now = std::time::Instant::now();
        let ctx = netanvil_types::RequestContext {
            request_id: 7,
            core_id: 0,
            is_sampled: false,
            intended_time: now,
            actual_time: now,
            sent_time: now,
            dispatch_time: now,
            session_id: None,
        };
        let spec = gen.generate(&ctx);
        assert_eq!(spec.method, http::Method::POST);
        assert_eq!(spec.url, "http://example.com/api");
        let body = String::from_utf8(spec.body.unwrap()).unwrap();
        assert!(body.contains("\"id\":7"));
    }

    #[test]
    fn wants_responses_false_by_default() {
        let script = r#"
            function generate(ctx) {
                return { method: "GET", url: "http://x.com", headers: [], body: null };
            }
        "#;
        let gen = V8Generator::<netanvil_types::HttpRequestSpec>::new(script, &[]).unwrap();
        assert!(!gen.wants_responses());
    }

    #[test]
    fn wants_responses_true_when_defined() {
        let script = r#"
            function generate(ctx) {
                return { method: "GET", url: "http://x.com", headers: [], body: null };
            }
            function on_response(result) {}
        "#;
        let gen = V8Generator::<netanvil_types::HttpRequestSpec>::new(script, &[]).unwrap();
        assert!(gen.wants_responses());
    }

    #[test]
    fn response_config_detection() {
        let script = r#"
            function generate(ctx) {
                return { method: "GET", url: "http://x.com", headers: [], body: null };
            }
            function on_response(result) {}
            function response_config() { return { headers: false, body: true }; }
        "#;
        let gen = V8Generator::<netanvil_types::HttpRequestSpec>::new(script, &[]).unwrap();
        assert!(gen.wants_responses());
        assert!(!gen.response_cfg.headers);
        assert!(gen.response_cfg.body);
    }

    #[test]
    fn update_targets() {
        let script = r#"
            var targets = [];
            function init(t) { targets = t; }
            function generate(ctx) {
                return { method: "GET", url: targets[0], headers: [], body: null };
            }
            function update_targets(t) { targets = t; }
        "#;
        let mut gen =
            V8Generator::<netanvil_types::HttpRequestSpec>::new(script, &["http://old.com".into()])
                .unwrap();
        let now = std::time::Instant::now();
        let ctx = netanvil_types::RequestContext {
            request_id: 0,
            core_id: 0,
            is_sampled: false,
            intended_time: now,
            actual_time: now,
            sent_time: now,
            dispatch_time: now,
            session_id: None,
        };
        assert_eq!(gen.generate(&ctx).url, "http://old.com");
        gen.update_targets(vec!["http://new.com".into()]);
        assert_eq!(gen.generate(&ctx).url, "http://new.com");
    }

    #[test]
    fn fallback_on_bad_script() {
        let script = r#"
            function generate(ctx) { return "not an object"; }
        "#;
        let mut gen = V8Generator::<netanvil_types::HttpRequestSpec>::new(script, &[]).unwrap();
        let now = std::time::Instant::now();
        let ctx = netanvil_types::RequestContext {
            request_id: 0,
            core_id: 0,
            is_sampled: false,
            intended_time: now,
            actual_time: now,
            sent_time: now,
            dispatch_time: now,
            session_id: None,
        };
        assert_eq!(gen.generate(&ctx).url, "http://error.invalid");
    }

    #[test]
    fn missing_generate_fails() {
        let script = r#"var x = 1;"#;
        assert!(V8Generator::<netanvil_types::HttpRequestSpec>::new(script, &[]).is_err());
    }

    #[test]
    fn contexts_are_isolated() {
        let script_a = r#"
            var state = "A";
            function generate(ctx) {
                return { method: "GET", url: "http://" + state, headers: [], body: null };
            }
        "#;
        let script_b = r#"
            var state = "B";
            function generate(ctx) {
                return { method: "GET", url: "http://" + state, headers: [], body: null };
            }
        "#;
        let now = std::time::Instant::now();
        let ctx = netanvil_types::RequestContext {
            request_id: 0,
            core_id: 0,
            is_sampled: false,
            intended_time: now,
            actual_time: now,
            sent_time: now,
            dispatch_time: now,
            session_id: None,
        };
        let mut gen_a = V8Generator::<netanvil_types::HttpRequestSpec>::new(script_a, &[]).unwrap();
        let mut gen_b = V8Generator::<netanvil_types::HttpRequestSpec>::new(script_b, &[]).unwrap();
        assert_eq!(gen_a.generate(&ctx).url, "http://A");
        assert_eq!(gen_b.generate(&ctx).url, "http://B");
        assert_eq!(gen_a.generate(&ctx).url, "http://A");
    }
}
