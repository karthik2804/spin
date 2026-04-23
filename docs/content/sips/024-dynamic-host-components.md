title = "SIP 024 - Dynamic Host Components"
template = "main"
date = "2025-04-10T12:00:00Z"

---

Summary: Allow users to provide arbitrary host interfaces at `spin up` time by supplying Wasm components whose exports are dynamically wired as imports available to guest application components. Configuration is transparent and flexible — each host component defines its own schema, and Spin passes TOML tables through as JSON without interpretation. The configure interface is defined by Spin (`spin:host-component/configure`) for consistency. Host components get optional read-write filesystem access for persistence.

# Background

Platforms that build on Spin often define custom interfaces beyond the WASI and Spin built-in capabilities. For example, a cloud platform might expose:

- A **regional cache** (`example:cache/store`) — shared across requests, backed by a distributed cache in production
- A **geo:ip lookup** (`platform:geo/ip`) — maps client IP addresses to geographic regions for content localization

Today, developers targeting these platforms cannot test their apps locally without either modifying the Spin codebase or using static composition (which doesn't support shared, stateful services).

## Motivating examples

**Cache**: A platform provides a regional cache. The cache is shared across all requests — state set by one request is visible to subsequent requests. For local testing, a developer provides a simple in-memory cache implementation. The operator controls the cache size (`max_bytes`).

**Geo:IP**: A platform provides IP-to-region mapping. For local testing, a developer provides a stub that always returns a configurable default region. The per-component config might specify which IP header to read from:

```toml
[host_component.geo]
default_region = "us-east-1"

[component.my-handler.host_component.geo]
ip_header = "X-Forwarded-For"
```

The host component interprets its own per-component config — Spin just passes it through.

## Related work

- **SIP 020 (Component Dependencies)** enables composing components at build time. Does not support shared, stateful host services.
- **Spin Factors** provide the plugin architecture for built-in host capabilities. This proposal extends the same system for user-provided capabilities.

# Proposal

## Overview

Dynamic host components are loaded via `--host-component` CLI flags, configured via `spin.toml` tables, and access-controlled per guest component. Each host component:

1. **Exports service interfaces** (e.g., `example:cache/store`) that become available as guest imports.
2. **Exports `spin:host-component/configure`** — a Spin-defined WIT interface with `get-key()` and `init(config) -> result<_, string>`.
3. **Receives config transparently** — Spin serializes the TOML table to JSON and passes it through. The host component decides what the config means (cache sizes, hostnames, access lists, etc.).
4. **Gets optional read-write filesystem access** for persistence (e.g., sqlite-backed caching).

Guest components declare which host components they can access via `[component.X.host_component.<key>]` tables in `spin.toml`. The content of these tables is opaque to Spin — each host component interprets it differently.

## The `spin:host-component/configure` interface

The configure interface is **defined by Spin** at `wit/deps/spin-host-component@0.1.0/configure.wit` to ensure consistency across all host components:

```wit
package spin:host-component@0.1.0;

interface configure {
    /// Returns the manifest key for configuration lookup.
    /// Spin looks for [host_component.<key>] and [component.X.host_component.<key>].
    get-key: func() -> string;

    /// Called once at startup with JSON-serialized configuration.
    /// Return an error string if the configuration is invalid.
    init: func(config: string) -> result<_, string>;
}
```

Host components **export** this interface. They do not define their own — they reference the Spin-provided definition:

```wit
world my-host-component {
    export my-service-interface;
    export spin:host-component/configure@0.1.0;
}
```

### `get-key() -> string`

The host component declares which `spin.toml` table it expects. Returning `"cache"` tells Spin to read `[host_component.cache]` for runtime config and check `[component.X.host_component.cache]` for per-component access.

### `init(config: string) -> result<_, string>`

Called once at startup with the serialized JSON config. The host component parses and validates the config, returning an error string if it's invalid. Spin propagates this error as a startup failure.

**Config is transparent** — Spin does not interpret the JSON. Different host components use it differently:
- A cache might read `{"max_bytes": 1048576, "eviction_policy": "lru"}`
- A geo:ip service might read `{"default_region": "us-east-1"}`
- A rate limiter might read `{"requests_per_minute": 100, "allowed_origins": ["example.com"]}`

## CLI syntax

```
spin up \
  --host-component cache=./cache-provider.wasm \
  --host-component geo=./geo-ip-stub.wasm \
  --host-component-config 'cache={"max_bytes":1048576}'
```

### `--host-component name=path`

- **name**: Label for log messages, config matching, and filesystem directories.
- **path**: Path to a Wasm component (wasm32-wasip2).

### `--host-component-config name=json_or_@file`

Override/supplement `spin.toml` config. Supports `@path/to/file.json` for file-based config.

### Config resolution

1. `[host_component.<key>]` from `spin.toml` (if present)
2. Merged with `--host-component-config` CLI values (CLI wins on conflicts)
3. Serialized as JSON → passed to `init()`
4. If no config from either source, `init()` is not called

## Manifest integration

### App-level config

```toml
[host_component.cache]
max_bytes = 32

[host_component.geo]
default_region = "us-east-1"
```

### Per-component access and config

```toml
[component.my-handler]
source = "handler.wasm"

# Grant access to cache — empty table means "access only, no extra config"
[component.my-handler.host_component.cache]

# Grant access to geo with per-component config
[component.my-handler.host_component.geo]
ip_header = "X-Forwarded-For"

[component.admin-panel]
source = "admin.wasm"
# No host_component entries — cannot use cache or geo
```

**The per-component table content is transparent to Spin.** The presence of the key grants access. The content (if any) is available for the host component to interpret however it wants — access lists, feature flags, per-tenant settings, etc.

## Filesystem access

Host components receive optional read-write filesystem access at `<state_dir>/host-components/<name>/`, mounted at `/data`:

```
.spin/
  host-components/
    cache/      ← read-write, mounted at /data
    geo/        ← read-write, mounted at /data
```

This enables sqlite databases, file-based caches, persistent state, etc.

## How it works

### Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                            spin up                                │
│                                                                   │
│  spin.toml ─┬─▶ [host_component.cache] ──▶ JSON ─▶ init() ─▶ Ok │
│             │                                                     │
│             └─▶ [component.guest.host_component.cache]            │
│                  = access granted (content is opaque to Spin)     │
│                                                                   │
│  ┌──────────────┐    import cache:store    ┌───────────────┐     │
│  │ Guest Wasm   │ ──────────────────────▶ │ cache-provider │     │
│  │ (spin-sdk)   │   (access-controlled)   │ (shared, with  │     │
│  │              │                         │  /data FS)     │     │
│  └──────────────┘                         └───────────────┘     │
└──────────────────────────────────────────────────────────────────┘
```

### Lifecycle

1. **`Factor::init()`** — Load and compile host components. Discover exported interfaces via `Component::component_type().exports()`. Register forwarding closures in the guest `Linker` via `func_new_async`. Skip `spin:host-component/configure` (not forwarded to guests).

2. **`Factor::configure_app()`** — For each host component:
   - Instantiate into a dedicated `Store` with WASI P2 async and optional `/data` filesystem preopens.
   - Call `get-key()` to discover the manifest key.
   - Read `[host_component.<key>]`, merge with CLI config, serialize to JSON, call `init()`.
   - If `init()` returns an error, fail startup with the error message.
   - Build per-component access control from `[component.X.host_component.<key>]` metadata.

3. **`Factor::prepare()`** — Compute allowed interfaces per guest component from `host_component` metadata.

4. **Guest request handling** — Forwarding closures acquire async `Mutex`, call `Func::call_async()` with dynamic `Val` parameters.

### Key design decisions

**Spin-defined configure interface**: The `spin:host-component/configure` WIT is defined in Spin's `wit/` directory. All host components use the same interface, ensuring consistency. Host components reference it rather than defining their own.

**Transparent configuration**: Spin serializes TOML tables to JSON and passes them through without interpretation. Each host component defines its own config schema. If the config is invalid, `init()` returns an error string.

**Error propagation from `init()`**: `init` returns `result<_, string>`, allowing host components to reject invalid configuration at startup. This prevents silent misconfiguration.

**Per-component config is also transparent**: The content of `[component.X.host_component.<key>]` is opaque to Spin. The presence of the key grants access; the content is available for the host component if it needs per-component settings. Different host components use this differently.

**Filesystem persistence enables restartability**: Since host components get write access to a persistent data directory, their state can survive restarts. This opens the possibility of restarting host components without restarting the entire Spin process (to be decided in future work).

**Async throughout**: Uses `wasm_component_model_async(true)` and WASI P2 async. Forward-compatible with WASI P3.

**Dynamic dispatch via `Val`**: No compile-time knowledge of host component interfaces required.

## Implementation

### `wit/deps/spin-host-component@0.1.0/configure.wit`

The Spin-defined configure interface. Host components export this.

### `crates/factor-host-components/src/service.rs`

- `HostComponentService` — `Store` + `Instance` + cached export indices. Async `call_func()`.
- `instantiate_host_component(engine, loaded, data_dir)` — WASI P2 async + optional `/data` filesystem.
- `configure_service()` — Calls `init(json)`, propagates `result` errors.
- `get_manifest_key()` — Calls `get-key()`.
- `is_configure_interface()` — Matches `spin:host-component/configure` (also legacy `/configure` suffix).

### `crates/factor-host-components/src/lib.rs`

- `new(components, configs, data_dir)`
- `configure_app()` — Instantiate, call `get-key`, merge configs, call `init`, build access control.
- `prepare()` — Per-component allowed interfaces.

### `crates/manifest/src/schema/v2.rs`

- `AppManifest.host_component: Map<String, toml::Table>`
- `Component.host_component: Map<String, toml::Table>`

### `crates/loader/src/local.rs`

- Serializes both app-level and per-component `host_component` tables into locked app metadata.

## Example: Cache provider

```wit
package example:cache@0.1.0;

interface store {
    variant error { store-not-found(string), other(string) }
    get: func(store-name: string, key: string) -> result<option<list<u8>>, error>;
    set: func(store-name: string, key: string, value: list<u8>, ttl-ms: option<u64>) -> result<_, error>;
    delete: func(store-name: string, key: string) -> result<_, error>;
    exists: func(store-name: string, key: string) -> result<bool, error>;
}

world cache-provider {
    export store;
    export spin:host-component/configure@0.1.0;
}
```

```rust
impl exports::spin::host_component::configure::Guest for CacheProvider {
    fn get_key() -> String { "cache".to_string() }

    fn init(config: String) -> Result<(), String> {
        match parse_u64_field(&config, "max_bytes") {
            Some(max_bytes) => {
                *MAX_BYTES.lock().unwrap() = max_bytes as usize;
                Ok(())
            }
            None if config.is_empty() => Ok(()), // no config is fine
            None => Err("expected 'max_bytes' in configuration".into()),
        }
    }
}
```

## Example: Guest app (using Spin SDK)

The guest app uses `spin-sdk` for HTTP handling and `wit-bindgen` only for the cache interface:

```rust
wit_bindgen::generate!({
    world: "cache-client",
    path: "cache-wit",
});

use example::cache::store as cache;
use spin_sdk::http::{IntoResponse, Response};
use spin_sdk::http_component;

#[http_component]
fn handle(req: http::Request<()>) -> anyhow::Result<impl IntoResponse> {
    let key = req.uri().path().trim_start_matches('/');
    match cache::get("default", key) {
        Ok(Some(val)) => Ok(Response::new(200, val)),
        Ok(None) => Ok(Response::new(404, "not found")),
        Err(e) => Ok(Response::new(500, format!("{e:?}"))),
    }
}
```

### `spin.toml`

```toml
spin_manifest_version = 2

[application]
name = "my-app"

[host_component.cache]
max_bytes = 67108864

[[trigger.http]]
route = "/..."
component = "my-handler"

[component.my-handler]
source = "handler.wasm"

[component.my-handler.host_component.cache]
```

### Running

```
spin up --host-component cache=cache-provider.wasm
```

## Example: Geo:IP stub

A platform exposes `platform:geo/ip` for geographic request routing. For local testing:

```wit
package platform:geo@1.0.0;

interface ip {
    record location {
        region: string,
        country: string,
        city: option<string>,
    }
    lookup: func(ip-address: string) -> option<location>;
}

world geo-stub {
    export ip;
    export spin:host-component/configure@0.1.0;
}
```

```toml
[host_component.geo]
default_region = "us-east-1"
default_country = "US"

[component.my-handler.host_component.geo]
ip_header = "X-Forwarded-For"
```

The stub returns the configured defaults for every lookup. In production, the real implementation could query something like maxmindDB

# Future work

- **Restartable host components**: Since host components have persistent filesystem access, they could potentially be restarted without restarting Spin — useful for updating host component implementations without downtime. This is to be decided.
- **Per-component config passthrough**: Extend the configure interface with a `configure-component(id: string, config: string)` function so host components can receive per-component table content (e.g., per-tenant rate limits).
- **Registry support**: Load host components from OCI registries.
- **Resource type support**: Extend dynamic forwarding to handle component-model resource types.
- **Structured config schema**: Host components export a WIT type describing expected configuration for validation and documentation.

# Alternatives considered

## Static composition with `wac`

Does not support shared, stateful services. A composed dependency gets a fresh instance per invocation.

## Guest-controlled configuration

Rejected — operational parameters should be controlled by the operator, not the guest.

## Per-component configure defined by each host component

Each host component could define its own configure interface. Rejected in favor of a single Spin-defined `spin:host-component/configure` interface for consistency — all host components use the same protocol, with flexible/transparent JSON config content.

## gRPC / IPC sidecar

Adds operational complexity, latency, and serialization overhead. In-process via wasmtime is simpler and sandbox-safe.
