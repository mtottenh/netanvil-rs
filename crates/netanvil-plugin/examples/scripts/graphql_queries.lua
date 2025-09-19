-- graphql_queries.lua: GraphQL endpoint load testing
--
-- Sends a mix of GraphQL queries and mutations to a single endpoint.
-- Demonstrates how to generate varied POST bodies against the same URL,
-- which is the typical GraphQL access pattern.
--
-- Usage:
--   netanvil-cli test --url http://api.example.com/graphql --plugin graphql_queries.lua

local targets = {}
local counter = 0

-- Query templates with varying complexity and cost.
local queries = {
    {
        weight = 5,
        name = "GetUser",
        query = '{"query":"query GetUser($id:ID!){user(id:$id){id name email}}","variables":{"id":"%d"}}',
        id_range = 100000,
    },
    {
        weight = 3,
        name = "ListProducts",
        query = '{"query":"query ListProducts($page:Int!){products(page:$page,limit:20){id name price category{name}}}","variables":{"page":%d}}',
        id_range = 500,
    },
    {
        weight = 1,
        name = "SearchOrders",
        query = '{"query":"query SearchOrders($userId:ID!,$status:String){orders(userId:$userId,status:$status){id total items{product{name}quantity}}}","variables":{"userId":"%d","status":"completed"}}',
        id_range = 100000,
    },
    {
        weight = 1,
        name = "CreateReview",
        query = '{"query":"mutation CreateReview($input:ReviewInput!){createReview(input:$input){id rating}}","variables":{"input":{"productId":"%d","rating":%d,"comment":"Load test review %d"}}}',
        id_range = 50000,
    },
}

-- Precompute cumulative weights
local total_weight = 0
local cumulative = {}
for i, q in ipairs(queries) do
    total_weight = total_weight + q.weight
    cumulative[i] = total_weight
end

function init(target_list)
    targets = target_list
    counter = 0
end

function generate(ctx)
    counter = counter + 1
    local base = targets[((counter - 1) % #targets) + 1]

    -- Weighted query selection
    local roll = (counter % total_weight) + 1
    local selected = queries[1]
    for i, cw in ipairs(cumulative) do
        if roll <= cw then
            selected = queries[i]
            break
        end
    end

    -- Fill in variables
    local id = (counter * 7 + ctx.core_id) % selected.id_range
    local rating = (counter % 5) + 1
    local body = string.format(selected.query, id, rating, counter)

    return {
        method = "POST",
        url = base,
        headers = {
            {"Content-Type", "application/json"},
            {"Accept", "application/json"},
            {"X-Operation-Name", selected.name},
        },
        body = body,
    }
end

function update_targets(new_targets)
    targets = new_targets
end
