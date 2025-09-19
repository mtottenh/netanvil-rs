-- Response handler generator for netanvil-rs benchmarks.
--
-- Demonstrates on_response() callback for stateful session simulation.
-- Tracks response statuses and extracts a session token from headers.

local targets = {}
local counter = 0
local token = nil
local ok_count = 0
local err_count = 0

function init(target_list)
    targets = target_list
    counter = 0
end

-- Declare what response data we need.
function response_config()
    return { headers = true, body = false }
end

-- Called after each response completes.
function on_response(result)
    if result.status and result.status == 200 then
        ok_count = ok_count + 1
        if result.headers then
            local t = result.headers["X-Session-Token"]
            if t then token = t end
        end
    else
        err_count = err_count + 1
    end
end

function generate(ctx)
    counter = counter + 1
    local idx = ((counter - 1) % #targets) + 1

    local headers = {
        {"Content-Type", "application/json"},
    }
    if token then
        headers[#headers + 1] = {"Authorization", "Bearer " .. token}
    end

    return {
        method = "GET",
        url = targets[idx] .. "/api/data?seq=" .. counter,
        headers = headers,
        body = nil
    }
end
