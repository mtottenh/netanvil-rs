-- Example Lua request generator for netanvil-rs.
--
-- This script is loaded by LuaGenerator. It must define a `generate(ctx)` function
-- that receives a context table and returns a request specification table.
--
-- Usage:
--   LuaGenerator::new(include_str!("scripts/generator.lua"), &targets)

-- Module state (persists across calls)
local targets = {}
local counter = 0

-- Called once at startup with the target URL list.
function init(target_list)
    targets = target_list
    counter = 0
end

-- Called per-request on the hot path.
-- ctx fields: request_id, core_id, is_sampled, session_id
-- Must return: { method, url, headers, body }
function generate(ctx)
    local idx = (counter % #targets) + 1
    local url = targets[idx] .. "?seq=" .. counter .. "&core=" .. ctx.core_id

    local body = string.format(
        '{"request_id":%d,"seq":%d,"core_id":%d}',
        ctx.request_id, counter, ctx.core_id
    )

    counter = counter + 1

    return {
        method = "GET",
        url = url,
        headers = {
            {"Content-Type", "application/json"},
            {"X-Request-ID", tostring(ctx.request_id)}
        },
        body = body
    }
end

-- Optional: update targets mid-test.
function update_targets(new_targets)
    targets = new_targets
    counter = 0
end
