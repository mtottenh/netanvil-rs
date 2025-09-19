-- url_patterns.lua: Parameterized URL generation with random substitution
--
-- Generates requests against URL patterns with randomized path segments.
-- Useful for testing caches, CDNs, or any system where request distribution
-- across URL space matters.
--
-- Usage:
--   netanvil-cli test --url http://cdn.example.com --plugin url_patterns.lua

local targets = {}
local counter = 0

-- Configurable URL patterns. Edit these for your test scenario.
local patterns = {
    { path = "/images/%08d/%d.jpg",   weight = 5, id_range = 1000000, variant_range = 10 },
    { path = "/video/segment/%06d/%04d.ts", weight = 3, id_range = 50000, variant_range = 2000 },
    { path = "/api/catalog/%d",       weight = 2, id_range = 5000,   variant_range = 1 },
}

-- Precompute cumulative weights for weighted random selection
local total_weight = 0
local cumulative = {}
for i, p in ipairs(patterns) do
    total_weight = total_weight + p.weight
    cumulative[i] = total_weight
end

function init(target_list)
    targets = target_list
    counter = 0
end

-- Simple PRNG (xorshift32) for deterministic randomness without OS calls.
local rng_state = 42
local function xorshift()
    local x = rng_state
    x = x ~ (x << 13)
    x = x ~ (x >> 17)
    x = x ~ (x << 5)
    -- Keep in 32-bit range
    x = x & 0xFFFFFFFF
    if x == 0 then x = 1 end
    rng_state = x
    return x
end

function generate(ctx)
    counter = counter + 1
    local base = targets[((counter - 1) % #targets) + 1]

    -- Seed PRNG from request context for reproducibility
    if counter == 1 then
        rng_state = ctx.core_id * 2654435761 + 1
    end

    -- Weighted pattern selection
    local roll = (xorshift() % total_weight) + 1
    local selected = patterns[1]
    for i, cw in ipairs(cumulative) do
        if roll <= cw then
            selected = patterns[i]
            break
        end
    end

    -- Generate random IDs within the pattern's range
    local id = xorshift() % selected.id_range
    local variant = xorshift() % selected.variant_range

    local path = string.format(selected.path, id, variant)

    return {
        method = "GET",
        url = base .. path,
        headers = {},
        body = nil,
    }
end

function update_targets(new_targets)
    targets = new_targets
end
