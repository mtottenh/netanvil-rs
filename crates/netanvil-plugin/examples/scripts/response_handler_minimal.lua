-- Minimal response handler — status check only, no headers/body.
--
-- This represents the optimal fast path: on_response receives only
-- the fixed numeric fields (40-byte RawResult for WASM, 5 table
-- mutations for Lua). No variable data serialized.

local targets = {}
local counter = 0
local ok_count = 0

function init(target_list)
    targets = target_list
    counter = 0
end

-- Explicitly declare: no headers, no body needed.
function response_config()
    return { headers = false, body = false }
end

function on_response(result)
    if result.status == 200 then
        ok_count = ok_count + 1
    end
end

function generate(ctx)
    counter = counter + 1
    local idx = ((counter - 1) % #targets) + 1
    return {
        method = "GET",
        url = targets[idx] .. "/api/data?seq=" .. counter,
        headers = {},
        body = nil
    }
end
