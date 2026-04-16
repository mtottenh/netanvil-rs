use std::time::Duration;

use anyhow::{Context, Result};

use netanvil_core::{GeneratorFactory, GenericGeneratorFactory};
use netanvil_types::{
    BaselineMultiplier, BoundsConfig, ConnectionPolicy, ConstraintClassConfig, ConstraintConfig,
    CountDistribution, GainsConfig, HttpVersion, InternalMetric, MetricRef, PluginType, RateConfig,
    ResponseSignalConfig, SchedulerConfig, SetpointConstraintConfig, SignalAggregation,
    TargetMetric, ThresholdConstraintConfig, ThresholdSource, ValueDistribution, WarmupConfig,
    WeightedValue,
};

pub fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    if let Some(ms) = s.strip_suffix("ms") {
        let n: u64 = ms.parse().context("invalid milliseconds")?;
        return Ok(Duration::from_millis(n));
    }
    if let Some(secs) = s.strip_suffix('s') {
        let n: f64 = secs.parse().context("invalid seconds")?;
        return Ok(Duration::from_secs_f64(n));
    }
    if let Some(mins) = s.strip_suffix('m') {
        let n: f64 = mins.parse().context("invalid minutes")?;
        return Ok(Duration::from_secs_f64(n * 60.0));
    }
    let n: f64 = s.parse().context("invalid duration")?;
    Ok(Duration::from_secs_f64(n))
}

pub fn parse_header(s: &str) -> Result<(String, String)> {
    let (name, value) = s
        .split_once(':')
        .context("header must be in 'Name: Value' format")?;
    Ok((name.trim().to_string(), value.trim().to_string()))
}

pub fn parse_scheduler(s: &str) -> Result<SchedulerConfig> {
    match s.to_lowercase().as_str() {
        "constant" | "const" => Ok(SchedulerConfig::ConstantRate),
        "poisson" => Ok(SchedulerConfig::Poisson { seed: None }),
        other => anyhow::bail!("unknown scheduler: {other} (use 'constant' or 'poisson')"),
    }
}

pub fn parse_target_metric(s: &str) -> Result<TargetMetric> {
    // Check for "external:name" prefix first
    if let Some(name) = s.strip_prefix("external:") {
        return Ok(TargetMetric::External {
            name: name.to_string(),
        });
    }
    match s.to_lowercase().as_str() {
        "latency-p50" | "p50" => Ok(TargetMetric::LatencyP50),
        "latency-p90" | "p90" => Ok(TargetMetric::LatencyP90),
        "latency-p99" | "p99" => Ok(TargetMetric::LatencyP99),
        "error-rate" | "errors" => Ok(TargetMetric::ErrorRate),
        "throughput-send" | "throughput" | "send-mbps" => Ok(TargetMetric::ThroughputSend),
        "throughput-recv" | "recv-mbps" => Ok(TargetMetric::ThroughputRecv),
        other => anyhow::bail!(
            "unknown PID metric: {other} (use 'latency-p50', 'latency-p90', 'latency-p99', \
             'error-rate', 'throughput-send', 'throughput-recv', or 'external:<name>')"
        ),
    }
}

/// Parsed constraint: metric and limit value.
pub struct ParsedConstraint {
    pub metric: TargetMetric,
    pub limit: f64,
}

/// Parse a PID constraint string: "metric < value" or "metric > value".
/// Only '<' (upper limit) is supported for now.
pub fn parse_pid_constraint(s: &str) -> Result<ParsedConstraint> {
    let parts: Vec<&str> = s.split('<').collect();
    if parts.len() != 2 {
        anyhow::bail!(
            "constraint must be in 'metric < value' format (e.g. 'latency-p99 < 500'), got: {s}"
        );
    }
    let metric = parse_target_metric(parts[0].trim())?;
    let limit: f64 = parts[1]
        .trim()
        .parse()
        .context("invalid numeric limit in constraint")?;
    Ok(ParsedConstraint { metric, limit })
}

/// Parse step definitions: "0s:100,5s:500,10s:200"
pub fn parse_steps(s: &str) -> Result<Vec<(Duration, f64)>> {
    let mut steps = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        let (time_str, rps_str) = part
            .split_once(':')
            .context("step must be in 'duration:rps' format (e.g. '5s:500')")?;
        let time = parse_duration(time_str.trim())?;
        let rps: f64 = rps_str.trim().parse().context("invalid RPS in step")?;
        steps.push((time, rps));
    }
    if steps.is_empty() {
        anyhow::bail!("at least one step is required");
    }
    Ok(steps)
}

pub fn parse_connection_policy(
    s: &str,
    persistent_ratio: f64,
    conn_lifetime: Option<&str>,
) -> Result<ConnectionPolicy> {
    match s.to_lowercase().as_str() {
        "keepalive" | "keep-alive" => Ok(ConnectionPolicy::KeepAlive),
        "always-new" | "new" => Ok(ConnectionPolicy::AlwaysNew),
        "mixed" => {
            if !(0.0..=1.0).contains(&persistent_ratio) {
                anyhow::bail!("--persistent-ratio must be between 0.0 and 1.0");
            }
            let lifetime = conn_lifetime.map(parse_count_distribution).transpose()?;
            Ok(ConnectionPolicy::Mixed {
                persistent_ratio,
                connection_lifetime: lifetime,
            })
        }
        other => anyhow::bail!(
            "unknown connection policy: {other} (use 'keepalive', 'always-new', or 'mixed')"
        ),
    }
}

/// Parse a count distribution: "fixed:100", "uniform:50,200", "normal:100,20",
/// "weighted:50@30,100@50,200@20"
pub fn parse_count_distribution(s: &str) -> Result<CountDistribution> {
    let (kind, params) = s
        .split_once(':')
        .context("distribution format: 'fixed:N', 'uniform:min,max', 'normal:mean,stddev', or 'weighted:val@pct,...'")?;
    match kind.to_lowercase().as_str() {
        "fixed" => {
            let n: u32 = params
                .trim()
                .parse()
                .context("fixed:N requires an integer")?;
            Ok(CountDistribution::Fixed(n))
        }
        "uniform" => {
            let (min_s, max_s) = params
                .split_once(',')
                .context("uniform requires min,max (e.g. 'uniform:50,200')")?;
            let min: u32 = min_s.trim().parse().context("invalid min")?;
            let max: u32 = max_s.trim().parse().context("invalid max")?;
            if min > max {
                anyhow::bail!("uniform min ({min}) must be <= max ({max})");
            }
            Ok(CountDistribution::Uniform { min, max })
        }
        "normal" => {
            let (mean_s, stddev_s) = params
                .split_once(',')
                .context("normal requires mean,stddev (e.g. 'normal:100,20')")?;
            let mean: f64 = mean_s.trim().parse().context("invalid mean")?;
            let stddev: f64 = stddev_s.trim().parse().context("invalid stddev")?;
            Ok(CountDistribution::Normal { mean, stddev })
        }
        "weighted" => {
            let entries = parse_weighted_entries::<u32>(params)?;
            Ok(CountDistribution::Weighted(entries))
        }
        "exponential" | "exp" => {
            let mean: f64 = params
                .trim()
                .parse()
                .context("exponential:mean requires a number")?;
            Ok(CountDistribution::Exponential { mean })
        }
        "lognormal" | "log-normal" => {
            let (mu_s, sigma_s) = params
                .split_once(',')
                .context("lognormal requires mu,sigma (e.g. 'lognormal:5.0,0.5')")?;
            let mu: f64 = mu_s.trim().parse().context("invalid mu")?;
            let sigma: f64 = sigma_s.trim().parse().context("invalid sigma")?;
            Ok(CountDistribution::LogNormal { mu, sigma })
        }
        "pareto" => {
            let (scale_s, shape_s) = params
                .split_once(',')
                .context("pareto requires scale,shape (e.g. 'pareto:10,2')")?;
            let scale: f64 = scale_s.trim().parse().context("invalid scale")?;
            let shape: f64 = shape_s.trim().parse().context("invalid shape")?;
            Ok(CountDistribution::Pareto { scale, shape })
        }
        "zipf" => {
            let (n_s, exp_s) = params
                .split_once(',')
                .context("zipf requires n,exponent (e.g. 'zipf:1000,1.0')")?;
            let n: u64 = n_s.trim().parse().context("invalid n")?;
            let exponent: f64 = exp_s.trim().parse().context("invalid exponent")?;
            Ok(CountDistribution::Zipf { n, exponent })
        }
        other => {
            anyhow::bail!("unknown distribution: {other} (use 'fixed', 'uniform', 'normal', 'weighted', 'exponential', 'lognormal', 'pareto', or 'zipf')")
        }
    }
}

/// Parse a size distribution for u16: bare integer "1200", or
/// "fixed:1200", "uniform:200,1500", "normal:800,200", "weighted:200@30,1200@50,1500@20"
pub fn parse_u16_distribution(s: &str) -> Result<ValueDistribution<u16>> {
    // Try bare integer first (backward compatible)
    if let Ok(n) = s.trim().parse::<u16>() {
        return Ok(ValueDistribution::Fixed(n));
    }
    let (kind, params) = s
        .split_once(':')
        .context("size distribution: bare integer, 'fixed:N', 'uniform:min,max', 'normal:mean,stddev', or 'weighted:val@pct,...'")?;
    match kind.to_lowercase().as_str() {
        "fixed" => {
            let n: u16 = params
                .trim()
                .parse()
                .context("fixed:N requires an integer")?;
            Ok(ValueDistribution::Fixed(n))
        }
        "uniform" => {
            let (min_s, max_s) = params.split_once(',').context("uniform requires min,max")?;
            let min: u16 = min_s.trim().parse().context("invalid min")?;
            let max: u16 = max_s.trim().parse().context("invalid max")?;
            if min > max {
                anyhow::bail!("uniform min ({min}) must be <= max ({max})");
            }
            Ok(ValueDistribution::Uniform { min, max })
        }
        "normal" => {
            let (mean_s, stddev_s) = params
                .split_once(',')
                .context("normal requires mean,stddev")?;
            let mean: f64 = mean_s.trim().parse().context("invalid mean")?;
            let stddev: f64 = stddev_s.trim().parse().context("invalid stddev")?;
            Ok(ValueDistribution::Normal { mean, stddev })
        }
        "weighted" => {
            let entries = parse_weighted_entries::<u16>(params)?;
            Ok(ValueDistribution::Weighted(entries))
        }
        "exponential" | "exp" => {
            let mean: f64 = params
                .trim()
                .parse()
                .context("exponential:mean requires a number")?;
            Ok(ValueDistribution::Exponential { mean })
        }
        "lognormal" | "log-normal" => {
            let (mu_s, sigma_s) = params
                .split_once(',')
                .context("lognormal requires mu,sigma")?;
            let mu: f64 = mu_s.trim().parse().context("invalid mu")?;
            let sigma: f64 = sigma_s.trim().parse().context("invalid sigma")?;
            Ok(ValueDistribution::LogNormal { mu, sigma })
        }
        "pareto" => {
            let (scale_s, shape_s) = params
                .split_once(',')
                .context("pareto requires scale,shape")?;
            let scale: f64 = scale_s.trim().parse().context("invalid scale")?;
            let shape: f64 = shape_s.trim().parse().context("invalid shape")?;
            Ok(ValueDistribution::Pareto { scale, shape })
        }
        "zipf" => {
            let (n_s, exp_s) = params.split_once(',').context("zipf requires n,exponent")?;
            let n: u64 = n_s.trim().parse().context("invalid n")?;
            let exponent: f64 = exp_s.trim().parse().context("invalid exponent")?;
            Ok(ValueDistribution::Zipf { n, exponent })
        }
        other => anyhow::bail!("unknown distribution: {other}"),
    }
}

/// Parse a size distribution for u32: bare integer "1200", or
/// "fixed:1200", "uniform:200,1500", "normal:800,200", "weighted:200@30,1200@50,1500@20"
pub fn parse_u32_distribution(s: &str) -> Result<ValueDistribution<u32>> {
    // Try bare integer first (backward compatible)
    if let Ok(n) = s.trim().parse::<u32>() {
        return Ok(ValueDistribution::Fixed(n));
    }
    let (kind, params) = s
        .split_once(':')
        .context("size distribution: bare integer, 'fixed:N', 'uniform:min,max', 'normal:mean,stddev', or 'weighted:val@pct,...'")?;
    match kind.to_lowercase().as_str() {
        "fixed" => {
            let n: u32 = params
                .trim()
                .parse()
                .context("fixed:N requires an integer")?;
            Ok(ValueDistribution::Fixed(n))
        }
        "uniform" => {
            let (min_s, max_s) = params.split_once(',').context("uniform requires min,max")?;
            let min: u32 = min_s.trim().parse().context("invalid min")?;
            let max: u32 = max_s.trim().parse().context("invalid max")?;
            if min > max {
                anyhow::bail!("uniform min ({min}) must be <= max ({max})");
            }
            Ok(ValueDistribution::Uniform { min, max })
        }
        "normal" => {
            let (mean_s, stddev_s) = params
                .split_once(',')
                .context("normal requires mean,stddev")?;
            let mean: f64 = mean_s.trim().parse().context("invalid mean")?;
            let stddev: f64 = stddev_s.trim().parse().context("invalid stddev")?;
            Ok(ValueDistribution::Normal { mean, stddev })
        }
        "weighted" => {
            let entries = parse_weighted_entries::<u32>(params)?;
            Ok(ValueDistribution::Weighted(entries))
        }
        "exponential" | "exp" => {
            let mean: f64 = params
                .trim()
                .parse()
                .context("exponential:mean requires a number")?;
            Ok(ValueDistribution::Exponential { mean })
        }
        "lognormal" | "log-normal" => {
            let (mu_s, sigma_s) = params
                .split_once(',')
                .context("lognormal requires mu,sigma")?;
            let mu: f64 = mu_s.trim().parse().context("invalid mu")?;
            let sigma: f64 = sigma_s.trim().parse().context("invalid sigma")?;
            Ok(ValueDistribution::LogNormal { mu, sigma })
        }
        "pareto" => {
            let (scale_s, shape_s) = params
                .split_once(',')
                .context("pareto requires scale,shape")?;
            let scale: f64 = scale_s.trim().parse().context("invalid scale")?;
            let shape: f64 = shape_s.trim().parse().context("invalid shape")?;
            Ok(ValueDistribution::Pareto { scale, shape })
        }
        "zipf" => {
            let (n_s, exp_s) = params.split_once(',').context("zipf requires n,exponent")?;
            let n: u64 = n_s.trim().parse().context("invalid n")?;
            let exponent: f64 = exp_s.trim().parse().context("invalid exponent")?;
            Ok(ValueDistribution::Zipf { n, exponent })
        }
        other => anyhow::bail!("unknown distribution: {other}"),
    }
}

/// Parse weighted entries: "200@30,1200@50,1500@20" → Vec<WeightedValue<T>>
fn parse_weighted_entries<T: std::str::FromStr>(s: &str) -> Result<Vec<WeightedValue<T>>>
where
    T::Err: std::fmt::Display,
{
    let mut entries = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        let (val_s, weight_s) = part
            .split_once('@')
            .context("weighted entries: 'value@weight,...' (e.g. '200@30,1200@50,1500@20')")?;
        let value: T = val_s
            .trim()
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid value '{val_s}': {e}"))?;
        let weight: f64 = weight_s
            .trim()
            .parse()
            .context(format!("invalid weight '{weight_s}'"))?;
        if weight <= 0.0 {
            anyhow::bail!("weight must be positive, got {weight}");
        }
        entries.push(WeightedValue::new(value, weight));
    }
    if entries.is_empty() {
        anyhow::bail!("weighted distribution requires at least one entry");
    }
    Ok(entries)
}

/// Parse a human-friendly bandwidth string into bits per second.
///
/// Accepts:
/// - "56k" or "56K" -> 56,000 bps
/// - "1m" or "1M" -> 1,000,000 bps
/// - "10m" -> 10,000,000 bps
/// - "1g" or "1G" -> 1,000,000,000 bps
/// - "56000" -> 56,000 bps (raw number)
/// - "1.5m" -> 1,500,000 bps (fractional)
pub fn parse_bandwidth(s: &str) -> Result<u64> {
    let s = s.trim();
    if s.is_empty() {
        anyhow::bail!("empty bandwidth string");
    }

    let (num_str, multiplier) = if let Some(n) = s.strip_suffix(['k', 'K']) {
        (n, 1_000u64)
    } else if let Some(n) = s.strip_suffix("kbps") {
        (n, 1_000u64)
    } else if let Some(n) = s.strip_suffix(['m', 'M']) {
        (n, 1_000_000u64)
    } else if let Some(n) = s.strip_suffix("mbps") {
        (n, 1_000_000u64)
    } else if let Some(n) = s.strip_suffix(['g', 'G']) {
        (n, 1_000_000_000u64)
    } else if let Some(n) = s.strip_suffix("gbps") {
        (n, 1_000_000_000u64)
    } else {
        (s, 1u64)
    };

    let num: f64 = num_str
        .trim()
        .parse()
        .context(format!("invalid bandwidth number: '{num_str}'"))?;
    let bps = (num * multiplier as f64) as u64;
    if bps == 0 {
        anyhow::bail!("bandwidth must be > 0");
    }
    Ok(bps)
}

/// Format bandwidth in human-readable form.
pub fn format_bandwidth(bps: u64) -> String {
    if bps >= 1_000_000_000 {
        format!("{:.1} Gbps", bps as f64 / 1_000_000_000.0)
    } else if bps >= 1_000_000 {
        format!("{:.1} Mbps", bps as f64 / 1_000_000.0)
    } else if bps >= 1_000 {
        format!("{:.0} kbps", bps as f64 / 1_000.0)
    } else {
        format!("{bps} bps")
    }
}

/// Parse an HTTP version string from the CLI into an [`HttpVersion`] enum.
///
/// Accepted values: "1.1", "1", "http1" → Http1; "2", "h2" → Http2;
/// "2c", "h2c" → Http2c; "auto" → Auto.
pub fn parse_http_version(s: &str) -> Result<HttpVersion> {
    match s.to_lowercase().trim() {
        "1.1" | "1" | "http1" | "http/1.1" => Ok(HttpVersion::Http1),
        "2" | "h2" | "http2" | "http/2" => Ok(HttpVersion::Http2),
        "2c" | "h2c" | "http2c" => Ok(HttpVersion::Http2c),
        "auto" | "negotiate" => Ok(HttpVersion::Auto),
        other => anyhow::bail!(
            "unknown HTTP version: '{other}'. Use \"1.1\", \"2\", \"2c\", or \"auto\""
        ),
    }
}

/// Detect plugin type from file extension or explicit --plugin-type flag.
pub fn detect_plugin_type(path: &str, explicit: &str) -> Result<PluginType> {
    if explicit != "auto" {
        return match explicit.to_lowercase().as_str() {
            "hybrid" => Ok(PluginType::Hybrid),
            "lua" | "luajit" => Ok(PluginType::Lua),
            "wasm" => Ok(PluginType::Wasm),
            "js" | "v8" | "javascript" => Ok(PluginType::Js),
            other => {
                anyhow::bail!(
                    "unknown --plugin-type: {other} (use 'hybrid', 'lua', 'wasm', or 'js')"
                )
            }
        };
    }
    // Auto-detect from extension
    if path.ends_with(".wasm") {
        Ok(PluginType::Wasm)
    } else if path.ends_with(".lua") {
        Ok(PluginType::Lua)
    } else if path.ends_with(".js") {
        Ok(PluginType::Js)
    } else {
        anyhow::bail!("cannot auto-detect plugin type for '{path}'. Use --plugin-type to specify.")
    }
}

/// Build a generator factory from a plugin file.
///
/// Returns a closure suitable for `TestBuilder::generator_factory()`.
/// Each call creates a new generator instance for one core.
pub fn build_plugin_factory(
    plugin_path: &str,
    plugin_type: PluginType,
    targets: &[String],
) -> Result<GeneratorFactory> {
    match plugin_type {
        PluginType::Hybrid => {
            let script = std::fs::read_to_string(plugin_path)
                .context(format!("failed to read plugin: {plugin_path}"))?;
            let config = netanvil_plugin_luajit::config_from_lua(&script)
                .map_err(|e| anyhow::anyhow!("hybrid config error: {e}"))?;
            Ok(Box::new(move |_core_id| {
                Box::new(netanvil_plugin::HybridGenerator::new(config.clone()))
                    as Box<
                        dyn netanvil_types::RequestGenerator<
                            Spec = netanvil_types::HttpRequestSpec,
                        >,
                    >
            }))
        }
        PluginType::Lua => {
            let script = std::fs::read_to_string(plugin_path)
                .context(format!("failed to read plugin: {plugin_path}"))?;
            let targets = targets.to_vec();
            Ok(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin_luajit::LuaJitGenerator::new(&script, &targets)
                        .expect("LuaJIT generator init failed"),
                )
                    as Box<
                        dyn netanvil_types::RequestGenerator<
                            Spec = netanvil_types::HttpRequestSpec,
                        >,
                    >
            }))
        }
        PluginType::Wasm => {
            let wasm_bytes = std::fs::read(plugin_path)
                .context(format!("failed to read WASM module: {plugin_path}"))?;
            let (engine, module) = netanvil_plugin::compile_wasm_module(&wasm_bytes)
                .map_err(|e| anyhow::anyhow!("WASM compile error: {e}"))?;
            let targets = targets.to_vec();
            Ok(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin::WasmGenerator::new(&engine, &module, &targets)
                        .expect("WASM generator init failed"),
                )
                    as Box<
                        dyn netanvil_types::RequestGenerator<
                            Spec = netanvil_types::HttpRequestSpec,
                        >,
                    >
            }))
        }
        PluginType::Js => {
            #[cfg(feature = "v8")]
            {
                let script = std::fs::read_to_string(plugin_path)
                    .context(format!("failed to read plugin: {plugin_path}"))?;
                let targets = targets.to_vec();
                Ok(Box::new(move |_core_id| {
                    Box::new(
                        netanvil_plugin_v8::V8Generator::new(&script, &targets)
                            .expect("V8 generator init failed"),
                    )
                        as Box<
                            dyn netanvil_types::RequestGenerator<
                                Spec = netanvil_types::HttpRequestSpec,
                            >,
                        >
                }))
            }
            #[cfg(not(feature = "v8"))]
            {
                anyhow::bail!(
                    "V8/JS plugin support requires the 'v8' feature flag. \
                     Rebuild with: cargo build --features v8"
                )
            }
        }
    }
}

/// Build a TCP generator factory from a plugin file.
///
/// Same pattern as [`build_plugin_factory`] but produces generators for
/// [`netanvil_types::TcpRequestSpec`] instead of HTTP.
pub fn build_tcp_plugin_factory(
    plugin_path: &str,
    plugin_type: PluginType,
    targets: &[String],
) -> Result<GenericGeneratorFactory<netanvil_types::TcpRequestSpec>> {
    match plugin_type {
        PluginType::Hybrid => {
            anyhow::bail!("hybrid plugins are only supported for HTTP")
        }
        PluginType::Lua => {
            let script = std::fs::read_to_string(plugin_path)
                .context(format!("failed to read plugin: {plugin_path}"))?;
            let targets = targets.to_vec();
            Ok(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin_luajit::LuaJitGenerator::<netanvil_types::TcpRequestSpec>::new(
                        &script, &targets,
                    )
                    .expect("LuaJIT generator init failed"),
                )
                    as Box<
                        dyn netanvil_types::RequestGenerator<Spec = netanvil_types::TcpRequestSpec>,
                    >
            }))
        }
        PluginType::Wasm => {
            let wasm_bytes = std::fs::read(plugin_path)
                .context(format!("failed to read WASM module: {plugin_path}"))?;
            let (engine, module) = netanvil_plugin::compile_wasm_module(&wasm_bytes)
                .map_err(|e| anyhow::anyhow!("WASM compile error: {e}"))?;
            let targets = targets.to_vec();
            Ok(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin::WasmGenerator::<netanvil_types::TcpRequestSpec>::new(
                        &engine, &module, &targets,
                    )
                    .expect("WASM generator init failed"),
                )
                    as Box<
                        dyn netanvil_types::RequestGenerator<Spec = netanvil_types::TcpRequestSpec>,
                    >
            }))
        }
        PluginType::Js => {
            #[cfg(feature = "v8")]
            {
                let script = std::fs::read_to_string(plugin_path)
                    .context(format!("failed to read plugin: {plugin_path}"))?;
                let targets = targets.to_vec();
                Ok(Box::new(move |_core_id| {
                    Box::new(
                        netanvil_plugin_v8::V8Generator::<netanvil_types::TcpRequestSpec>::new(
                            &script, &targets,
                        )
                        .expect("V8 generator init failed"),
                    )
                        as Box<
                            dyn netanvil_types::RequestGenerator<
                                Spec = netanvil_types::TcpRequestSpec,
                            >,
                        >
                }))
            }
            #[cfg(not(feature = "v8"))]
            {
                anyhow::bail!(
                    "V8/JS plugin support requires the 'v8' feature flag. \
                     Rebuild with: cargo build --features v8"
                )
            }
        }
    }
}

/// Build a UDP generator factory from a plugin file.
///
/// Same pattern as [`build_plugin_factory`] but produces generators for
/// [`netanvil_types::UdpRequestSpec`] instead of HTTP.
pub fn build_udp_plugin_factory(
    plugin_path: &str,
    plugin_type: PluginType,
    targets: &[String],
) -> Result<GenericGeneratorFactory<netanvil_types::UdpRequestSpec>> {
    match plugin_type {
        PluginType::Hybrid => {
            anyhow::bail!("hybrid plugins are only supported for HTTP")
        }
        PluginType::Lua => {
            let script = std::fs::read_to_string(plugin_path)
                .context(format!("failed to read plugin: {plugin_path}"))?;
            let targets = targets.to_vec();
            Ok(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin_luajit::LuaJitGenerator::<netanvil_types::UdpRequestSpec>::new(
                        &script, &targets,
                    )
                    .expect("LuaJIT generator init failed"),
                )
                    as Box<
                        dyn netanvil_types::RequestGenerator<Spec = netanvil_types::UdpRequestSpec>,
                    >
            }))
        }
        PluginType::Wasm => {
            let wasm_bytes = std::fs::read(plugin_path)
                .context(format!("failed to read WASM module: {plugin_path}"))?;
            let (engine, module) = netanvil_plugin::compile_wasm_module(&wasm_bytes)
                .map_err(|e| anyhow::anyhow!("WASM compile error: {e}"))?;
            let targets = targets.to_vec();
            Ok(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin::WasmGenerator::<netanvil_types::UdpRequestSpec>::new(
                        &engine, &module, &targets,
                    )
                    .expect("WASM generator init failed"),
                )
                    as Box<
                        dyn netanvil_types::RequestGenerator<Spec = netanvil_types::UdpRequestSpec>,
                    >
            }))
        }
        PluginType::Js => {
            #[cfg(feature = "v8")]
            {
                let script = std::fs::read_to_string(plugin_path)
                    .context(format!("failed to read plugin: {plugin_path}"))?;
                let targets = targets.to_vec();
                Ok(Box::new(move |_core_id| {
                    Box::new(
                        netanvil_plugin_v8::V8Generator::<netanvil_types::UdpRequestSpec>::new(
                            &script, &targets,
                        )
                        .expect("V8 generator init failed"),
                    )
                        as Box<
                            dyn netanvil_types::RequestGenerator<
                                Spec = netanvil_types::UdpRequestSpec,
                            >,
                        >
                }))
            }
            #[cfg(not(feature = "v8"))]
            {
                anyhow::bail!(
                    "V8/JS plugin support requires the 'v8' feature flag. \
                     Rebuild with: cargo build --features v8"
                )
            }
        }
    }
}

/// Build a DNS generator factory from a plugin file.
///
/// Same pattern as [`build_plugin_factory`] but produces generators for
/// [`netanvil_types::DnsRequestSpec`] instead of HTTP.
pub fn build_dns_plugin_factory(
    plugin_path: &str,
    plugin_type: PluginType,
    targets: &[String],
) -> Result<GenericGeneratorFactory<netanvil_types::DnsRequestSpec>> {
    match plugin_type {
        PluginType::Hybrid => {
            anyhow::bail!("hybrid plugins are only supported for HTTP")
        }
        PluginType::Lua => {
            let script = std::fs::read_to_string(plugin_path)
                .context(format!("failed to read plugin: {plugin_path}"))?;
            let targets = targets.to_vec();
            Ok(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin_luajit::LuaJitGenerator::<netanvil_types::DnsRequestSpec>::new(
                        &script, &targets,
                    )
                    .expect("LuaJIT generator init failed"),
                )
                    as Box<
                        dyn netanvil_types::RequestGenerator<Spec = netanvil_types::DnsRequestSpec>,
                    >
            }))
        }
        PluginType::Wasm => {
            let wasm_bytes = std::fs::read(plugin_path)
                .context(format!("failed to read WASM module: {plugin_path}"))?;
            let (engine, module) = netanvil_plugin::compile_wasm_module(&wasm_bytes)
                .map_err(|e| anyhow::anyhow!("WASM compile error: {e}"))?;
            let targets = targets.to_vec();
            Ok(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin::WasmGenerator::<netanvil_types::DnsRequestSpec>::new(
                        &engine, &module, &targets,
                    )
                    .expect("WASM generator init failed"),
                )
                    as Box<
                        dyn netanvil_types::RequestGenerator<Spec = netanvil_types::DnsRequestSpec>,
                    >
            }))
        }
        PluginType::Js => {
            #[cfg(feature = "v8")]
            {
                let script = std::fs::read_to_string(plugin_path)
                    .context(format!("failed to read plugin: {plugin_path}"))?;
                let targets = targets.to_vec();
                Ok(Box::new(move |_core_id| {
                    Box::new(
                        netanvil_plugin_v8::V8Generator::<netanvil_types::DnsRequestSpec>::new(
                            &script, &targets,
                        )
                        .expect("V8 generator init failed"),
                    )
                        as Box<
                            dyn netanvil_types::RequestGenerator<
                                Spec = netanvil_types::DnsRequestSpec,
                            >,
                        >
                }))
            }
            #[cfg(not(feature = "v8"))]
            {
                anyhow::bail!(
                    "V8/JS plugin support requires the 'v8' feature flag. \
                     Rebuild with: cargo build --features v8"
                )
            }
        }
    }
}

/// Build a Redis generator factory from a plugin file.
///
/// Same pattern as [`build_plugin_factory`] but produces generators for
/// [`netanvil_types::RedisRequestSpec`] instead of HTTP.
pub fn build_redis_plugin_factory(
    plugin_path: &str,
    plugin_type: PluginType,
    targets: &[String],
) -> Result<GenericGeneratorFactory<netanvil_types::RedisRequestSpec>> {
    match plugin_type {
        PluginType::Hybrid => {
            anyhow::bail!("hybrid plugins are only supported for HTTP")
        }
        PluginType::Lua => {
            let script = std::fs::read_to_string(plugin_path)
                .context(format!("failed to read plugin: {plugin_path}"))?;
            let targets = targets.to_vec();
            Ok(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin_luajit::LuaJitGenerator::<netanvil_types::RedisRequestSpec>::new(
                        &script, &targets,
                    )
                    .expect("LuaJIT generator init failed"),
                )
                    as Box<
                        dyn netanvil_types::RequestGenerator<Spec = netanvil_types::RedisRequestSpec>,
                    >
            }))
        }
        PluginType::Wasm => {
            let wasm_bytes = std::fs::read(plugin_path)
                .context(format!("failed to read WASM module: {plugin_path}"))?;
            let (engine, module) = netanvil_plugin::compile_wasm_module(&wasm_bytes)
                .map_err(|e| anyhow::anyhow!("WASM compile error: {e}"))?;
            let targets = targets.to_vec();
            Ok(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin::WasmGenerator::<netanvil_types::RedisRequestSpec>::new(
                        &engine, &module, &targets,
                    )
                    .expect("WASM generator init failed"),
                )
                    as Box<
                        dyn netanvil_types::RequestGenerator<
                            Spec = netanvil_types::RedisRequestSpec,
                        >,
                    >
            }))
        }
        PluginType::Js => {
            #[cfg(feature = "v8")]
            {
                let script = std::fs::read_to_string(plugin_path)
                    .context(format!("failed to read plugin: {plugin_path}"))?;
                let targets = targets.to_vec();
                Ok(Box::new(move |_core_id| {
                    Box::new(
                        netanvil_plugin_v8::V8Generator::<netanvil_types::RedisRequestSpec>::new(
                            &script, &targets,
                        )
                        .expect("V8 generator init failed"),
                    )
                        as Box<
                            dyn netanvil_types::RequestGenerator<
                                Spec = netanvil_types::RedisRequestSpec,
                            >,
                        >
                }))
            }
            #[cfg(not(feature = "v8"))]
            {
                anyhow::bail!(
                    "V8/JS plugin support requires the 'v8' feature flag. \
                     Rebuild with: cargo build --features v8"
                )
            }
        }
    }
}

/// PID-specific arguments for building a rate config.
pub struct PidArgs<'a> {
    pub metric: &'a str,
    pub target: f64,
    pub kp: Option<f64>,
    pub ki: Option<f64>,
    pub kd: Option<f64>,
    pub min_rps: f64,
    pub max_rps: f64,
    pub constraints: &'a [String],
    pub autotune_duration: Duration,
}

/// Ramp-specific CLI arguments.
pub struct RampArgs {
    pub warmup_duration: Duration,
    pub latency_multiplier: f64,
    pub max_error_rate: f64,
    /// Floor applied to the observed baseline before multiplying (milliseconds).
    /// Prevents sub-millisecond services from getting thresholds tighter than
    /// the OS jitter envelope. Default: 2.0ms.
    pub baseline_floor_ms: f64,
}

/// Adaptive-mode shortcut arguments for common use cases.
#[derive(Default)]
pub struct AdaptiveShortcutArgs {
    /// --latency-limit: Threshold constraint on p99 latency (milliseconds).
    /// Creates a Threshold constraint that backs off when latency exceeds this value.
    pub latency_limit_ms: Option<f64>,
    /// --error-rate-limit: Threshold constraint on error rate (percentage).
    pub error_rate_limit_pct: Option<f64>,
    /// --latency-setpoint: PID setpoint tracking for p99 latency (milliseconds).
    /// Adjusts rate to maintain latency at this target.
    pub latency_setpoint_ms: Option<f64>,
    /// --rate-config: Load full Adaptive config from a JSON/TOML file.
    pub config_file: Option<String>,
}

pub fn build_rate_config(
    rate_mode: &str,
    rps: f64,
    steps: Option<&str>,
    pid: &PidArgs<'_>,
    ramp: &RampArgs,
    adaptive: &AdaptiveShortcutArgs,
) -> Result<RateConfig> {
    match rate_mode.to_lowercase().as_str() {
        "static" | "const" => Ok(RateConfig::Static { rps }),
        "step" => {
            let steps_str = steps.context("--steps required for step rate mode")?;
            let steps = parse_steps(steps_str)?;
            Ok(RateConfig::Step { steps })
        }
        "pid" => {
            anyhow::bail!(
                "rate mode 'pid' has been replaced. Use --rate-mode adaptive with \
                 --pid-constraint flags, or see docs for equivalent config."
            )
        }
        "ramp" => {
            anyhow::bail!(
                "rate mode 'ramp' has been replaced. Use --rate-mode adaptive, \
                 or see docs for equivalent config."
            )
        }
        "adaptive" => {
            // If --rate-config is specified, load the full Adaptive config from file.
            if let Some(ref path) = adaptive.config_file {
                let contents = std::fs::read_to_string(path)
                    .with_context(|| format!("failed to read rate config file '{path}'"))?;
                let config: RateConfig = serde_json::from_str(&contents)
                    .with_context(|| format!("failed to parse JSON rate config from '{path}'"))?;
                return Ok(config);
            }

            // Build from shortcut flags + defaults.
            let warmup_rps = rps.clamp(1.0, 100.0);
            let mut constraints: Vec<ConstraintConfig> = Vec::new();

            // Safety constraints (always present).
            constraints.push(ConstraintConfig::Threshold(ThresholdConstraintConfig {
                id: "timeout".into(),
                metric: MetricRef::Internal(InternalMetric::TimeoutFraction),
                smoother: None,
                class_override: Some(ConstraintClassConfig::Catastrophic),
                threshold_source: ThresholdSource::Absolute { threshold: 0.01 },
                persistence: 1,
                self_caused_cap: None,
                backoff: None,
            }));
            constraints.push(ConstraintConfig::Threshold(ThresholdConstraintConfig {
                id: "inflight".into(),
                metric: MetricRef::Internal(InternalMetric::InFlightDropFraction),
                smoother: None,
                class_override: Some(ConstraintClassConfig::Catastrophic),
                threshold_source: ThresholdSource::Absolute { threshold: 0.01 },
                persistence: 1,
                self_caused_cap: None,
                backoff: None,
            }));

            // --latency-limit: absolute p99 threshold (don't exceed this).
            if let Some(limit_ms) = adaptive.latency_limit_ms {
                constraints.push(ConstraintConfig::Threshold(ThresholdConstraintConfig {
                    id: "latency".into(),
                    metric: MetricRef::Internal(InternalMetric::LatencyP99),
                    smoother: None,
                    class_override: None,
                    threshold_source: ThresholdSource::Absolute {
                        threshold: limit_ms,
                    },
                    persistence: 2,
                    self_caused_cap: Some(1.5),
                    backoff: None,
                }));
            } else if adaptive.latency_setpoint_ms.is_none() {
                // Default: latency from baseline (like old ramp mode).
                constraints.push(ConstraintConfig::Threshold(ThresholdConstraintConfig {
                    id: "latency".into(),
                    metric: MetricRef::Internal(InternalMetric::LatencyP99),
                    smoother: None,
                    class_override: None,
                    threshold_source: ThresholdSource::FromBaseline {
                        threshold_from_baseline: BaselineMultiplier {
                            multiplier: ramp.latency_multiplier,
                            baseline_floor_ms: ramp.baseline_floor_ms,
                        },
                    },
                    persistence: 2,
                    self_caused_cap: Some(1.5),
                    backoff: None,
                }));
            }

            // --latency-setpoint: PID tracking (maintain this latency precisely).
            if let Some(setpoint_ms) = adaptive.latency_setpoint_ms {
                constraints.push(ConstraintConfig::Setpoint(SetpointConstraintConfig {
                    id: "latency_setpoint".into(),
                    metric: MetricRef::Internal(InternalMetric::LatencyP99),
                    smoother: None,
                    target: setpoint_ms,
                    gains: GainsConfig::Auto {
                        autotune_duration: pid.autotune_duration,
                        smoothing: 0.3,
                    },
                    tracking_gain: 0.5,
                }));
            }

            // --error-rate-limit: error rate threshold (percentage).
            let error_threshold = adaptive.error_rate_limit_pct.unwrap_or(ramp.max_error_rate);
            constraints.push(ConstraintConfig::Threshold(ThresholdConstraintConfig {
                id: "error_rate".into(),
                metric: MetricRef::Internal(InternalMetric::ErrorRate),
                smoother: None,
                class_override: None,
                threshold_source: ThresholdSource::Absolute {
                    threshold: error_threshold,
                },
                persistence: 1,
                self_caused_cap: None,
                backoff: None,
            }));

            Ok(RateConfig::Adaptive {
                bounds: BoundsConfig {
                    min_rps: pid.min_rps,
                    max_rps: pid.max_rps,
                },
                warmup: Some(WarmupConfig {
                    rps: warmup_rps,
                    duration: ramp.warmup_duration,
                }),
                initial_rps: None,
                constraints,
                increase: None,
                cooldown: None,
                floor: None,
                rate_change_limits: None,
            })
        }
        other => {
            anyhow::bail!(
                "unknown rate mode: {other} (use 'static', 'step', 'pid', 'ramp', or 'adaptive')"
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Hex encoding helpers
// ---------------------------------------------------------------------------

pub fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[allow(dead_code)]
pub fn decode_hex(s: &str) -> Result<Vec<u8>> {
    let clean: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    (0..clean.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&clean[i..i + 2], 16)
                .map_err(|e| anyhow::anyhow!("hex decode at {i}: {e}"))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Multi-protocol support
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DetectedProtocol {
    Http,
    Tcp,
    Udp,
    Dns,
    Redis,
}

pub fn detect_protocol(urls: &[String]) -> DetectedProtocol {
    let first = urls.first().map(|s| s.as_str()).unwrap_or("http://");
    if first.starts_with("tcp://") {
        DetectedProtocol::Tcp
    } else if first.starts_with("udp://") {
        DetectedProtocol::Udp
    } else if first.starts_with("dns://") {
        DetectedProtocol::Dns
    } else if first.starts_with("redis://") {
        DetectedProtocol::Redis
    } else {
        DetectedProtocol::Http
    }
}

/// Parse tcp:// or udp:// URLs to SocketAddr, resolving DNS if needed.
pub fn parse_socket_targets(urls: &[String], scheme: &str) -> Result<Vec<std::net::SocketAddr>> {
    urls.iter()
        .map(|u| {
            let addr_str = u
                .strip_prefix(scheme)
                .ok_or_else(|| anyhow::anyhow!("URL {u} does not start with {scheme}"))?;
            use std::net::ToSocketAddrs;
            addr_str
                .to_socket_addrs()
                .context(format!("failed to resolve {addr_str}"))?
                .next()
                .ok_or_else(|| anyhow::anyhow!("no addresses found for {addr_str}"))
        })
        .collect()
}

/// Resolve payload from --payload, --payload-hex, or --payload-file flags.
pub fn resolve_payload(
    text: &Option<String>,
    hex: &Option<String>,
    file: &Option<String>,
) -> Result<Vec<u8>> {
    if let Some(t) = text {
        // Unescape common sequences
        let unescaped = t
            .replace("\\r", "\r")
            .replace("\\n", "\n")
            .replace("\\t", "\t");
        Ok(unescaped.into_bytes())
    } else if let Some(h) = hex {
        let clean: String = h.chars().filter(|c| !c.is_whitespace()).collect();
        (0..clean.len())
            .step_by(2)
            .map(|i| {
                u8::from_str_radix(&clean[i..i + 2], 16)
                    .context(format!("invalid hex at position {i}"))
            })
            .collect()
    } else if let Some(path) = file {
        std::fs::read(path).context(format!("failed to read payload file: {path}"))
    } else {
        Ok(Vec::new())
    }
}

/// Parse --response-signal flags into ResponseSignalConfig.
///
/// Format: "Header-Name" or "Header-Name:aggregation"
/// where aggregation is "mean" (default), "max", or "last".
pub fn parse_response_signals(signals: &[String]) -> Result<Vec<ResponseSignalConfig>> {
    signals
        .iter()
        .map(|s| {
            let (header, agg) = if let Some((h, a)) = s.rsplit_once(':') {
                let aggregation = match a.to_lowercase().as_str() {
                    "mean" => SignalAggregation::Mean,
                    "max" => SignalAggregation::Max,
                    "last" => SignalAggregation::Last,
                    other => {
                        anyhow::bail!("unknown aggregation '{other}', expected: mean, max, last")
                    }
                };
                (h.to_string(), aggregation)
            } else {
                (s.clone(), SignalAggregation::Mean)
            };
            Ok(ResponseSignalConfig {
                header,
                signal_name: None,
                aggregation: agg,
            })
        })
        .collect()
}

/// Parse --mode flag into TcpTestMode.
pub fn parse_tcp_mode(s: &str) -> Result<netanvil_tcp::TcpTestMode> {
    match s.to_lowercase().as_str() {
        "echo" => Ok(netanvil_tcp::TcpTestMode::Echo),
        "rr" => Ok(netanvil_tcp::TcpTestMode::RR),
        "stream" | "sink" => Ok(netanvil_tcp::TcpTestMode::Sink),
        "maerts" | "source" => Ok(netanvil_tcp::TcpTestMode::Source),
        "bidir" => Ok(netanvil_tcp::TcpTestMode::Bidir),
        "crr" => Ok(netanvil_tcp::TcpTestMode::CRR),
        other => {
            anyhow::bail!("unknown mode: {other}. Expected: echo, rr, stream, maerts, bidir, crr")
        }
    }
}

/// Parse --framing flag into TcpFraming.
pub fn parse_tcp_framing(s: &str, delimiter: &str) -> Result<netanvil_tcp::TcpFraming> {
    if s == "raw" {
        Ok(netanvil_tcp::TcpFraming::Raw)
    } else if let Some(rest) = s.strip_prefix("length-prefix:") {
        let width: u8 = rest
            .parse()
            .context("length-prefix width must be 1, 2, or 4")?;
        if !matches!(width, 1 | 2 | 4) {
            anyhow::bail!("length-prefix width must be 1, 2, or 4, got {width}");
        }
        Ok(netanvil_tcp::TcpFraming::LengthPrefixed { width })
    } else if s == "delimiter" {
        let bytes = delimiter
            .replace("\\r", "\r")
            .replace("\\n", "\n")
            .into_bytes();
        Ok(netanvil_tcp::TcpFraming::Delimiter(bytes))
    } else if let Some(rest) = s.strip_prefix("fixed:") {
        let size: usize = rest.parse().context("fixed size must be a number")?;
        Ok(netanvil_tcp::TcpFraming::FixedSize(size))
    } else {
        anyhow::bail!("unknown framing: {s}. Expected: raw, length-prefix:N, delimiter, fixed:N")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_seconds() {
        assert_eq!(parse_duration("10s").unwrap(), Duration::from_secs(10));
        assert_eq!(
            parse_duration("1.5s").unwrap(),
            Duration::from_secs_f64(1.5)
        );
    }

    #[test]
    fn parse_duration_milliseconds() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
    }

    #[test]
    fn parse_duration_minutes() {
        assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
    }

    #[test]
    fn parse_duration_bare_number() {
        assert_eq!(parse_duration("30").unwrap(), Duration::from_secs(30));
    }

    #[test]
    fn parse_header_valid() {
        let (name, value) = parse_header("Content-Type: application/json").unwrap();
        assert_eq!(name, "Content-Type");
        assert_eq!(value, "application/json");
    }

    #[test]
    fn parse_header_invalid() {
        assert!(parse_header("no-colon-here").is_err());
    }

    #[test]
    fn parse_scheduler_variants() {
        assert!(matches!(
            parse_scheduler("constant").unwrap(),
            SchedulerConfig::ConstantRate
        ));
        assert!(matches!(
            parse_scheduler("poisson").unwrap(),
            SchedulerConfig::Poisson { .. }
        ));
        assert!(parse_scheduler("invalid").is_err());
    }

    #[test]
    fn parse_steps_valid() {
        let steps = parse_steps("0s:100,5s:500,10s:200").unwrap();
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0], (Duration::from_secs(0), 100.0));
        assert_eq!(steps[1], (Duration::from_secs(5), 500.0));
        assert_eq!(steps[2], (Duration::from_secs(10), 200.0));
    }

    #[test]
    fn parse_steps_with_ms() {
        let steps = parse_steps("0s:50, 500ms:200").unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[1].0, Duration::from_millis(500));
    }

    #[test]
    fn parse_steps_invalid() {
        assert!(parse_steps("not-a-step").is_err());
        assert!(parse_steps("").is_err());
    }

    #[test]
    fn parse_target_metric_variants() {
        assert!(matches!(
            parse_target_metric("latency-p99").unwrap(),
            TargetMetric::LatencyP99
        ));
        assert!(matches!(
            parse_target_metric("p50").unwrap(),
            TargetMetric::LatencyP50
        ));
        assert!(matches!(
            parse_target_metric("error-rate").unwrap(),
            TargetMetric::ErrorRate
        ));
        assert!(parse_target_metric("bogus").is_err());
    }

    #[test]
    fn build_static_rate() {
        let rate = build_rate_config(
            "static",
            500.0,
            None,
            &PidArgs {
                metric: "p99",
                target: 200.0,
                kp: None,
                ki: None,
                kd: None,
                min_rps: 10.0,
                max_rps: 10000.0,
                constraints: &[],
                autotune_duration: Duration::from_secs(3),
            },
            &RampArgs {
                warmup_duration: Duration::from_secs(10),
                latency_multiplier: 3.0,
                max_error_rate: 5.0,
                baseline_floor_ms: 4.0,
            },
            &AdaptiveShortcutArgs::default(),
        )
        .unwrap();
        assert!(matches!(rate, RateConfig::Static { rps } if rps == 500.0));
    }

    #[test]
    fn build_step_rate() {
        let rate = build_rate_config(
            "step",
            0.0,
            Some("0s:100,5s:500"),
            &PidArgs {
                metric: "p99",
                target: 200.0,
                kp: None,
                ki: None,
                kd: None,
                min_rps: 10.0,
                max_rps: 10000.0,
                constraints: &[],
                autotune_duration: Duration::from_secs(3),
            },
            &RampArgs {
                warmup_duration: Duration::from_secs(10),
                latency_multiplier: 3.0,
                max_error_rate: 5.0,
                baseline_floor_ms: 4.0,
            },
            &AdaptiveShortcutArgs::default(),
        )
        .unwrap();
        assert!(matches!(rate, RateConfig::Step { steps } if steps.len() == 2));
    }

    #[test]
    fn build_step_rate_requires_steps() {
        let result = build_rate_config(
            "step",
            0.0,
            None,
            &PidArgs {
                metric: "p99",
                target: 200.0,
                kp: None,
                ki: None,
                kd: None,
                min_rps: 10.0,
                max_rps: 10000.0,
                constraints: &[],
                autotune_duration: Duration::from_secs(3),
            },
            &RampArgs {
                warmup_duration: Duration::from_secs(10),
                latency_multiplier: 3.0,
                max_error_rate: 5.0,
                baseline_floor_ms: 4.0,
            },
            &AdaptiveShortcutArgs::default(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn build_pid_rate_mode_returns_error() {
        let result = build_rate_config(
            "pid",
            500.0,
            None,
            &PidArgs {
                metric: "latency-p99",
                target: 200.0,
                kp: Some(0.1),
                ki: Some(0.01),
                kd: Some(0.05),
                min_rps: 10.0,
                max_rps: 10000.0,
                constraints: &[],
                autotune_duration: Duration::from_secs(3),
            },
            &RampArgs {
                warmup_duration: Duration::from_secs(10),
                latency_multiplier: 3.0,
                max_error_rate: 5.0,
                baseline_floor_ms: 4.0,
            },
            &AdaptiveShortcutArgs::default(),
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("replaced"),
            "error should mention replacement: {err}"
        );
    }

    #[test]
    fn build_ramp_rate_mode_returns_error() {
        let result = build_rate_config(
            "ramp",
            100.0,
            None,
            &PidArgs {
                metric: "latency-p99",
                target: 200.0,
                kp: None,
                ki: None,
                kd: None,
                min_rps: 10.0,
                max_rps: 10000.0,
                constraints: &[],
                autotune_duration: Duration::from_secs(3),
            },
            &RampArgs {
                warmup_duration: Duration::from_secs(10),
                latency_multiplier: 3.0,
                max_error_rate: 5.0,
                baseline_floor_ms: 4.0,
            },
            &AdaptiveShortcutArgs::default(),
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("replaced"),
            "error should mention replacement: {err}"
        );
    }

    #[test]
    fn build_adaptive_rate() {
        let rate = build_rate_config(
            "adaptive",
            100.0,
            None,
            &PidArgs {
                metric: "latency-p99",
                target: 200.0,
                kp: None,
                ki: None,
                kd: None,
                min_rps: 10.0,
                max_rps: 50000.0,
                constraints: &[],
                autotune_duration: Duration::from_secs(3),
            },
            &RampArgs {
                warmup_duration: Duration::from_secs(10),
                latency_multiplier: 3.0,
                max_error_rate: 5.0,
                baseline_floor_ms: 4.0,
            },
            &AdaptiveShortcutArgs::default(),
        )
        .unwrap();
        match rate {
            RateConfig::Adaptive {
                bounds,
                warmup,
                constraints,
                ..
            } => {
                assert_eq!(bounds.min_rps, 10.0);
                assert_eq!(bounds.max_rps, 50000.0);
                assert!(warmup.is_some());
                assert_eq!(constraints.len(), 4); // timeout, inflight, latency, error_rate
            }
            _ => panic!("expected Adaptive"),
        }
    }

    #[test]
    fn parse_pid_constraint_valid() {
        let c = parse_pid_constraint("latency-p99 < 500").unwrap();
        assert!(matches!(c.metric, TargetMetric::LatencyP99));
        assert_eq!(c.limit, 500.0);
    }

    #[test]
    fn parse_pid_constraint_external() {
        let c = parse_pid_constraint("external:load < 80").unwrap();
        match &c.metric {
            TargetMetric::External { name } => assert_eq!(name, "load"),
            _ => panic!("expected External"),
        }
        assert_eq!(c.limit, 80.0);
    }

    #[test]
    fn parse_pid_constraint_invalid() {
        assert!(parse_pid_constraint("no-operator").is_err());
    }

    #[test]
    fn parse_target_metric_external() {
        let m = parse_target_metric("external:load").unwrap();
        assert!(matches!(m, TargetMetric::External { name } if name == "load"));
    }

    #[test]
    fn parse_count_distribution_fixed() {
        let d = parse_count_distribution("fixed:100").unwrap();
        assert!(matches!(d, CountDistribution::Fixed(100)));
    }

    #[test]
    fn parse_count_distribution_uniform() {
        let d = parse_count_distribution("uniform:50,200").unwrap();
        assert!(matches!(
            d,
            CountDistribution::Uniform { min: 50, max: 200 }
        ));
    }

    #[test]
    fn parse_count_distribution_normal() {
        let d = parse_count_distribution("normal:100,20").unwrap();
        match d {
            CountDistribution::Normal { mean, stddev } => {
                assert!((mean - 100.0).abs() < 0.01);
                assert!((stddev - 20.0).abs() < 0.01);
            }
            _ => panic!("expected Normal"),
        }
    }

    #[test]
    fn parse_count_distribution_invalid() {
        assert!(parse_count_distribution("bogus:1").is_err());
        assert!(parse_count_distribution("nocolon").is_err());
    }

    #[test]
    fn parse_connection_policy_variants() {
        assert!(matches!(
            parse_connection_policy("keepalive", 0.7, None).unwrap(),
            ConnectionPolicy::KeepAlive
        ));
        assert!(matches!(
            parse_connection_policy("always-new", 0.7, None).unwrap(),
            ConnectionPolicy::AlwaysNew
        ));
        let mixed = parse_connection_policy("mixed", 0.7, Some("fixed:100")).unwrap();
        match mixed {
            ConnectionPolicy::Mixed {
                persistent_ratio,
                connection_lifetime,
            } => {
                assert!((persistent_ratio - 0.7).abs() < 0.01);
                assert!(matches!(
                    connection_lifetime,
                    Some(CountDistribution::Fixed(100))
                ));
            }
            _ => panic!("expected Mixed"),
        }
    }
}
