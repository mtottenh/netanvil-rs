-- Example hybrid configuration script for netanvil-rs.
--
-- This script is called ONCE at setup time to produce a GeneratorConfig.
-- The hot path runs in native Rust using the returned configuration.
-- Zero per-request overhead from the scripting layer.
--
-- Usage:
--   let config = config_from_lua(include_str!("scripts/hybrid_config.lua"))?;
--   let generator = HybridGenerator::new(config);

function configure()
    return {
        method = "POST",
        url_patterns = {
            { pattern = "http://api.example.com/users/{request_id}?seq={seq}", weight = 3.0 },
            { pattern = "http://api.example.com/items/{seq}?core={core_id}", weight = 1.0 },
        },
        headers = {
            {"Content-Type", "application/json"},
            {"X-Test-Run", "netanvil-rs"},
        },
        body_template = '{"request_id":{request_id},"seq":{seq},"core":{core_id}}',
    }
end
