-- api_load_test.lua: Realistic REST API load test
--
-- Simulates a read-heavy API workload against a CRUD service:
--   80% GET (read), 10% POST (create), 5% PUT (update), 5% DELETE
--
-- Each request targets a different resource path with realistic payloads.
-- Demonstrates weighted request distribution and varied HTTP methods.
--
-- Usage:
--   netanvil-cli test --url http://api.example.com --plugin api_load_test.lua

local targets = {}
local counter = 0

function init(target_list)
    targets = target_list
    counter = 0
end

function generate(ctx)
    counter = counter + 1
    local base = targets[((counter - 1) % #targets) + 1]
    local roll = counter % 100

    local method, path, body

    if roll < 80 then
        -- 80%: GET a resource by ID
        local resource_id = (counter * 7 + ctx.core_id) % 100000
        method = "GET"
        path = "/api/v1/items/" .. resource_id
        body = nil

    elseif roll < 90 then
        -- 10%: POST (create)
        method = "POST"
        path = "/api/v1/items"
        body = string.format(
            '{"name":"item-%d","category":"cat-%d","price":%.2f}',
            counter, counter % 50, (counter % 10000) / 100.0
        )

    elseif roll < 95 then
        -- 5%: PUT (update)
        local resource_id = (counter * 13) % 100000
        method = "PUT"
        path = "/api/v1/items/" .. resource_id
        body = string.format(
            '{"name":"updated-%d","price":%.2f}',
            counter, (counter % 5000) / 100.0
        )

    else
        -- 5%: DELETE
        local resource_id = (counter * 17) % 100000
        method = "DELETE"
        path = "/api/v1/items/" .. resource_id
        body = nil
    end

    return {
        method = method,
        url = base .. path,
        headers = {
            {"Content-Type", "application/json"},
            {"Accept", "application/json"},
            {"X-Request-ID", tostring(ctx.request_id)},
        },
        body = body,
    }
end

function update_targets(new_targets)
    targets = new_targets
end
