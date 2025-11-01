// Response handler generator for netanvil-rs benchmarks.
//
// Demonstrates on_response() callback for stateful session simulation.
// Tracks response statuses and extracts a session token from headers.

var targets = [];
var counter = 0;
var token = null;
var ok_count = 0;
var err_count = 0;

function init(targetList) {
    targets = targetList;
    counter = 0;
}

// Declare what response data we need.
function response_config() {
    return { headers: true, body: false };
}

// Called after each response completes.
function on_response(result) {
    if (result.status && result.status === 200) {
        ok_count++;
        if (result.headers) {
            var t = result.headers["X-Session-Token"];
            if (t) { token = t; }
        }
    } else {
        err_count++;
    }
}

function generate(ctx) {
    counter++;
    var idx = (counter - 1) % targets.length;

    var headers = [
        ["Content-Type", "application/json"]
    ];
    if (token) {
        headers.push(["Authorization", "Bearer " + token]);
    }

    return {
        method: "GET",
        url: targets[idx] + "/api/data?seq=" + counter,
        headers: headers,
        body: null
    };
}
