-- dns_enumeration.lua: DNS subdomain enumeration load test
--
-- Generates DNS queries across a configurable set of subdomains and query
-- types. Useful for testing DNS server performance under varied query
-- patterns — authoritative servers, resolvers, or caching layers.
--
-- The plugin cycles through subdomain prefixes and query types, producing
-- a realistic mix of A, AAAA, MX, and TXT lookups across the domain space.
--
-- Usage:
--   netanvil-cli test --url dns://8.8.8.8:53 --plugin dns_enumeration.lua

local counter = 0

-- Subdomain prefixes to cycle through. Edit for your test scenario.
local subdomains = {
    "www", "api", "cdn", "mail", "smtp", "imap", "ns1", "ns2",
    "staging", "dev", "beta", "auth", "login", "dashboard",
    "static", "assets", "media", "img", "video", "docs",
    "status", "metrics", "grafana", "kibana", "elastic",
    "db", "redis", "cache", "queue", "worker",
}

-- Base domains to query against.
local domains = {
    "example.com",
    "example.org",
    "example.net",
}

-- Query type distribution: realistic mix weighted toward A records.
local query_types = {
    { type = "A",     weight = 50 },
    { type = "AAAA",  weight = 20 },
    { type = "MX",    weight = 10 },
    { type = "TXT",   weight = 10 },
    { type = "CNAME", weight = 5 },
    { type = "NS",    weight = 5 },
}

-- Precompute cumulative weights
local total_weight = 0
local cumulative = {}
for i, qt in ipairs(query_types) do
    total_weight = total_weight + qt.weight
    cumulative[i] = total_weight
end

function init(_target_list)
    counter = 0
end

function generate(ctx)
    counter = counter + 1

    -- Select subdomain and domain via round-robin
    local sub = subdomains[((counter - 1) % #subdomains) + 1]
    local domain = domains[((counter - 1) % #domains) + 1]
    local fqdn = sub .. "." .. domain

    -- Weighted query type selection
    local roll = (counter % total_weight) + 1
    local qtype = query_types[1].type
    for i, cw in ipairs(cumulative) do
        if roll <= cw then
            qtype = query_types[i].type
            break
        end
    end

    return {
        query_name = fqdn,
        query_type = qtype,
        recursion = true,
        dnssec = false,
    }
end
