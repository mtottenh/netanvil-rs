// Example JavaScript request generator for netanvil-rs.
//
// This script is loaded by V8Generator. It must define a `generate(ctx)` function
// that receives a context object and returns a request specification object.
//
// Requires the `v8` feature flag:
//   cargo run --features v8 --bin netanvil-cli -- test --plugin generator.js --url http://localhost:8080

// Module state (persists across calls)
var targets = [];
var counter = 0;

// Called once at startup with the target URL array.
function init(targetList) {
    targets = targetList;
    counter = 0;
}

// Called per-request on the hot path.
// ctx fields: request_id, core_id, is_sampled, session_id
// Must return: { method, url, headers, body }
function generate(ctx) {
    var idx = counter % targets.length;
    var url = targets[idx] + "?seq=" + counter + "&core=" + ctx.core_id;

    var body = '{"request_id":' + ctx.request_id + ',"seq":' + counter + ',"core_id":' + ctx.core_id + '}';

    counter++;

    return {
        method: "GET",
        url: url,
        headers: [
            ["Content-Type", "application/json"],
            ["X-Request-ID", String(ctx.request_id)]
        ],
        body: body
    };
}

// Optional: update targets mid-test.
function update_targets(newTargets) {
    targets = newTargets;
    counter = 0;
}
