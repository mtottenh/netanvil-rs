-- Session simulation generator for netanvil-rs.
--
-- Demonstrates capabilities IMPOSSIBLE with the hybrid plugin:
-- - Mixed HTTP methods (GET/POST) based on conditional logic
-- - Per-user Authorization headers rotating through a user pool
-- - Multi-step session flows (login -> browse -> purchase)
-- - Dynamic URL paths computed from internal state
--
-- Usage:
--   netanvil-cli test --url http://api.example.com --plugin session_generator.lua

local targets = {}
local counter = 0

-- State that persists across calls (per-core, not shared between cores)
local user_pool = {}

function init(target_list)
    targets = target_list
    counter = 0

    -- Build a pool of 10,000 test users
    for i = 1, 10000 do
        user_pool[i] = {
            id = i,
            name = "user-" .. i,
            token = "tok-" .. string.format("%08x", i * 31337),
        }
    end
end

function generate(ctx)
    counter = counter + 1

    -- Rotate through users (each core has its own pool index)
    local user_idx = ((counter - 1) % #user_pool) + 1
    local user = user_pool[user_idx]

    -- 3-step session: login (10%) -> browse (70%) -> purchase (20%)
    -- This is IMPOSSIBLE in hybrid mode: it requires conditional logic.
    local roll = counter % 10
    local method, path, body

    if roll == 0 then
        -- Step 1: Login (POST)
        method = "POST"
        path = "/api/v1/auth/login"
        body = string.format(
            '{"username":"%s","password":"test-pass-%d"}',
            user.name, user.id
        )
    elseif roll <= 7 then
        -- Step 2: Browse products (GET) -- no body
        local product_id = (counter * 7 + user.id) % 50000
        method = "GET"
        path = "/api/v1/products/" .. product_id
        body = nil
    else
        -- Step 3: Purchase (POST)
        local product_id = (counter * 13 + user.id) % 50000
        method = "POST"
        path = "/api/v1/orders"
        body = string.format(
            '{"user_id":%d,"product_id":%d,"quantity":%d}',
            user.id, product_id, (counter % 5) + 1
        )
    end

    local base_url = targets[((counter - 1) % #targets) + 1]

    return {
        method = method,
        url = base_url .. path,
        headers = {
            {"Content-Type", "application/json"},
            -- Dynamic per-user auth header: IMPOSSIBLE in hybrid.
            {"Authorization", "Bearer " .. user.token},
            {"X-Request-ID", tostring(ctx.request_id)},
            {"X-Session-Step", tostring(roll)},
        },
        body = body
    }
end

function update_targets(new_targets)
    targets = new_targets
end
