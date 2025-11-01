// Minimal response handler — status check only, no headers/body.
//
// Optimal fast path: on_response receives only the fixed numeric fields.

var targets = [];
var counter = 0;
var ok_count = 0;

function init(targetList) {
    targets = targetList;
    counter = 0;
}

// Explicitly declare: no headers, no body needed.
function response_config() {
    return { headers: false, body: false };
}

function on_response(result) {
    if (result.status === 200) {
        ok_count++;
    }
}

function generate(ctx) {
    counter++;
    var idx = (counter - 1) % targets.length;
    return {
        method: "GET",
        url: targets[idx] + "/api/data?seq=" + counter,
        headers: [],
        body: null
    };
}
