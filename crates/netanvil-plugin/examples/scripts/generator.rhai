// Example Rhai request generator for netanvil-rs.
//
// This script is loaded by RhaiGenerator. It must define a `generate(ctx)` function
// that receives an object map and returns an object map.
//
// Usage:
//   RhaiGenerator::new(include_str!("scripts/generator.rhai"), &targets)

// Note: `targets` and `counter` are injected into scope by RhaiGenerator::new()

// Called per-request on the hot path.
// ctx fields: request_id, core_id, is_sampled, session_id
// Must return: #{ method, url, headers, body }
fn generate(ctx) {
    let idx = counter % targets.len();
    let url = targets[idx] + "?seq=" + counter + "&core=" + ctx.core_id;

    let body = `{"request_id":` + ctx.request_id
             + `,"seq":` + counter
             + `,"core_id":` + ctx.core_id + "}";

    counter += 1;

    #{
        method: "GET",
        url: url,
        headers: [
            ["Content-Type", "application/json"],
            ["X-Request-ID", "" + ctx.request_id]
        ],
        body: body
    }
}
