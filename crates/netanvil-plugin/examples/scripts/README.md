# Plugin Examples

Example Lua, Rhai, and WASM request generators for netanvil-rs.

## Plugin Modes

netanvil-rs supports three plugin execution modes:

| Mode | Language | Per-request overhead | Use case |
|------|----------|---------------------|----------|
| **Hybrid** | Lua (config only) | 0 ns | Static URL patterns, weighted routing |
| **Lua** | LuaJIT | ~2 us | Dynamic logic, conditional paths, state |
| **Rhai** | Rhai | ~5 us | Sandboxed scripting, no C dependencies |
| **WASM** | Rust/C/etc | ~1 us | Maximum performance with full flexibility |

## Protocol Support

Plugins work across all supported protocols. The `generate(ctx)` return
table varies by protocol:

| Protocol | Return fields |
|----------|--------------|
| **HTTP** | `method`, `url`, `headers`, `body` |
| **DNS**  | `query_name`, `query_type`, `recursion`, `dnssec` |
| **TCP**  | `payload` (string or bytes) |
| **UDP**  | `payload` (string or bytes) |

Hybrid mode is HTTP-only. Lua, Rhai, and WASM plugins work with all protocols.

## Examples

### Getting Started

- **[generator.lua](generator.lua)** — Minimal Lua generator: round-robin URLs with JSON body. Start here to understand the plugin API contract.
- **[generator.rhai](generator.rhai)** — Same generator in Rhai syntax.
- **[hybrid_config.lua](hybrid_config.lua)** — Zero-overhead hybrid mode: URL patterns with weighted selection, evaluated entirely in native Rust.

### HTTP Workloads

- **[api_load_test.lua](api_load_test.lua)** — REST API CRUD simulation with weighted method distribution (80% GET, 10% POST, 5% PUT, 5% DELETE).
- **[session_generator.lua](session_generator.lua)** — Multi-step user session flow (login, browse, purchase) with per-user auth tokens. Demonstrates stateful logic impossible in hybrid mode.
- **[graphql_queries.lua](graphql_queries.lua)** — GraphQL endpoint testing with mixed queries and mutations of varying complexity.
- **[url_patterns.lua](url_patterns.lua)** — Parameterized URL generation with random path segments. Useful for cache/CDN testing where URL distribution matters.

### DNS Workloads

- **[dns_enumeration.lua](dns_enumeration.lua)** — Subdomain enumeration across multiple domains and query types (A, AAAA, MX, TXT, CNAME, NS) with weighted distribution.

### WASM

See [`../guest-wasm/`](../guest-wasm/) for a Rust-based WASM guest module implementing the binary RequestGenerator protocol.

## Plugin API

Every Lua/Rhai plugin must define a `generate(ctx)` function. The context
is the same across all protocols:

```lua
-- ctx fields:
--   request_id  (number)  — globally unique request ID
--   core_id     (number)  — worker core index (0-based)
--   is_sampled  (boolean) — true if this request is being sampled for detailed metrics
--   session_id  (number)  — session identifier for connection-aware generators
```

### HTTP plugins

```lua
function generate(ctx)
    return {
        method = "GET",
        url = "http://example.com/path",
        headers = {{"Content-Type", "application/json"}},
        body = nil,
    }
end
```

### DNS plugins

```lua
function generate(ctx)
    return {
        query_name = "www.example.com",
        query_type = "A",       -- A, AAAA, MX, CNAME, TXT, NS, SOA, PTR, SRV, ANY
        recursion = true,
        dnssec = false,
    }
end
```

### TCP/UDP plugins

```lua
function generate(ctx)
    return {
        payload = "PING\r\n",  -- string or raw bytes
    }
end
```

Optional lifecycle functions:

```lua
function init(target_list)       -- called once with --url values
function update_targets(targets) -- called when targets change mid-test
```

## Usage

```bash
# HTTP with Lua plugin
netanvil-cli test --url http://localhost:8080 --plugin api_load_test.lua

# HTTP with hybrid plugin (zero per-request overhead)
netanvil-cli test --url http://localhost:8080 --plugin hybrid_config.lua --plugin-type hybrid

# DNS with Lua plugin
netanvil-cli test --url dns://8.8.8.8:53 --plugin dns_enumeration.lua

# TCP with Lua plugin
netanvil-cli test --url tcp://localhost:6379 --plugin redis_commands.lua

# WASM plugin (any protocol)
netanvil-cli test --url http://localhost:8080 --plugin guest.wasm
```
