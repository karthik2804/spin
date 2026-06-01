title = "SIP 025 - C Shared Library Plugins"
template = "main"
date = "2025-05-29T12:00:00Z"

---

Summary: Allow users to provide host interfaces at `spin up` time by loading native C shared libraries (.so, .dylib, .dll). Each plugin exports a WIT definition and a dispatch function. Spin parses the WIT, registers the interfaces in the wasmtime linker, and forwards guest calls through a C FFI boundary using postcard binary serialization.

# Background

SIP 024 (Dynamic Host Components) enables loading Wasm components as host services. However, many platforms and use cases benefit from native host implementations:

- **Performance-critical services** — caching, rate limiting, and other hot-path operations where avoiding a Wasm runtime layer matters
- **Platform integrations** — Kubernetes, cloud provider SDKs, and OS-level APIs that are easier to implement in a native language
- **Language ecosystems** — C, Rust, Zig, and other systems languages where shared library compilation is the standard distribution format
- **Dependency constraints** — existing C libraries (SQLite, crypto, etc.) that are impractical to recompile as Wasm

Both SIP 024 and this proposal share the same high-level model: user-provided interfaces that are dynamically wired into the guest execution environment. They differ only in the implementation technology — Wasm components vs native shared libraries.

# Proposal

## Overview

C shared library plugins are loaded via `--plugin` CLI flags, configured via `--plugin-config`, and access-controlled per guest component. Each plugin:

1. **Is a native shared library** compiled from C, Rust (`cdylib`), Zig, or any language that produces a shared library with C ABI.
2. **Exports `plugin_wit_text()`** — returns the WIT definition as a null-terminated string.
3. **Exports `plugin_dispatch()`** — receives postcard-serialized parameters, returns postcard-serialized results.
4. **Optionally exports `plugin_init()`** — called at startup with configuration bytes.
5. **Optionally exports `plugin_cleanup()`** — called when the plugin is unloaded.

Guest components can call any function defined in the plugin's WIT exports. Spin handles all serialization, interface registration, and access control.

## C ABI

The ABI is defined in `crates/factor-plugins/include/spin_plugin_abi.h`:

```c
#include <stdint.h>

/**
 * Returns the WIT definition for this plugin.
 */
const char* plugin_wit_text(void);

/**
 * Initializes the plugin with configuration bytes.
 *
 * @param config_bytes  Pointer to config bytes (may be NULL if no config).
 * @param config_len    Length of config bytes. May be 0 if no config.
 * @return 0 on success, non-zero on error.
 */
int plugin_init(const uint8_t* config_bytes, int32_t config_len);

/**
 * Cleans up plugin resources.
 */
void plugin_cleanup(void);

/**
 * Dispatches a function call to the plugin.
 *
 * @param func_name        Fully qualified WIT function name (null-terminated).
 *                         Format: "package:interface@version#function"
 * @param params_bytes     postcard-serialized Vec<PluginVal> parameters.
 * @param params_len       Length of params bytes.
 * @param results_buf      Buffer to write postcard-serialized Vec<PluginVal> results.
 * @param results_buf_len  Size of results buffer in bytes.
 * @param results_len      Output: actual bytes written to results_buf.
 * @param error_buf        Buffer to write error message on failure.
 * @param error_buf_len    Size of error buffer in bytes.
 * @return 0 on success, non-zero on error.
 */
int plugin_dispatch(
    const char* func_name,
    const uint8_t* params_bytes,
    int32_t params_len,
    uint8_t* results_buf,
    int32_t results_buf_len,
    int32_t* results_len,
    char* error_buf,
    int32_t error_buf_len
);
```

### Design decisions

**Raw byte buffers with explicit length** — The ABI avoids string allocation entirely. Parameters and results are passed as raw `uint8_t*` buffers with explicit lengths. The host provides a results buffer; the plugin writes into it and returns the byte count.

**`const char*` for `plugin_wit_text`** — The WIT text is returned as a static C string (the shared library is expected to return a pointer to a `static const char[]`), avoiding any allocation or ownership transfer.

**Error reporting via buffer** — `plugin_dispatch` returns an integer status code and writes error messages into a provided buffer, avoiding heap allocation in the error path.

**No memory management responsibilities** — The plugin never frees memory allocated by the host, and the host never frees memory allocated by the plugin. All data is copied through the FFI boundary.

## Binary serialization: Postcard

Parameters and results cross the C boundary as [postcard](https://github.com/jamesmunns/postcard)-serialized bytes. Postcard is a no-std friendly bincode variant with:

- **Compact encoding** — no key strings, no type tags beyond discriminant bytes
- **Zero-copy potential** — supports deserializing from static buffers without heap allocation
- **Deterministic** — same input always produces same output
- **No stdlib dependency** — works in `#![no_std]` environments

### The `PluginVal` type

A shared ABI crate (`crates/plugin-plugin`, published as `spin-plugin-plugin`) defines a `PluginVal` enum that maps all `wasmtime::component::Val` variants to postcard-serializable types:

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum PluginVal {
    Bool(bool),
    S8(i8), U8(u8), S16(i16), U16(u16),
    S32(i32), U32(u32), S64(i64), U64(u64),
    Float32(f32), Float64(f64),
    Char(u32),
    String(Vec<u8>),
    List(Vec<PluginVal>),
    Record(Vec<(String, PluginVal)>),
    Tuple(Vec<PluginVal>),
    Variant(String, Option<Box<PluginVal>>),
    Enum(String),
    Option(Option<Box<PluginVal>>),
    Result(Option<Box<PluginVal>>, Option<Box<PluginVal>>),
    Flags(Vec<String>),
    Unit,
}
```

**Critical requirement**: This enum must be identical in both the host (`spin-plugin-plugin` crate) and the plugin code. The shared crate is designed to be imported by plugin authors as a dependency:

```toml
[dependencies]
spin-plugin-plugin = { path = "../../crates/plugin-plugin" }
```

The C plugin can either implement postcard serialization manually (as the reference C example does) or use a binding layer.

### Serialization flow

```
Guest calls cache::get("key")
    │
    ▼
wasmtime: params = [Val::String("key")]
    │
    ▼
Host factor: Val → PluginVal conversion
    │
    ▼
postcard::to_allocvec(&vec![PluginVal::String(b"key")]) → bytes
    │
    ▼
C FFI: plugin_dispatch("example:cache/store@0.1.1#get", bytes, len, ...)
    │
    ▼
Plugin: postcard::from_bytes::<Vec<PluginVal>>(params) → [PluginVal::String("key")]
    │
    ▼
Plugin: execute get("key") → PluginVal::Result(Ok(Some("value")), None)
    │
    ▼
Plugin: postcard::to_allocvec(&vec![result_val]) → bytes, write to results_buf
    │
    ▼
C FFI returns: bytes copied back to host
    │
    ▼
Host factor: postcard::from_bytes::<Vec<PluginVal>>(results) → [PluginVal::Result(...)]
    │
    ▼
PluginVal → wasmtime::Val conversion
    │
    ▼
wasmtime: writes result Val into guest call frame
```

## WIT parsing

The plugin returns its WIT definition as a string via `plugin_wit_text()`. Spin parses it using `wit-parser`:

```wit
package example:cache@0.1.1;

interface store {
    get: func(key: string) -> result<option<string>, string>;
    set: func(key: string, value: string) -> result<_, string>;
    delete: func(key: string) -> result<_, string>;
    exists: func(key: string) -> result<bool, string>;
}

world cache-plugin {
    export store;
}
```

Spin:

1. Parses the WIT text with `wit_parser::UnresolvedPackageGroup::parse_str()`.
2. Pushes the package into `wit_parser::Resolve`.
3. Iterates world exports to find `WorldKey::Interface` entries.
4. Constructs the fully qualified interface name as `namespace:name@version` (e.g., `example:cache/store@0.1.1`).
5. Collects function names per interface.
6. Registers each function in the wasmtime linker via `func_new_async`.

**Interface name construction is manual** to preserve the exact version from the WIT package declaration, as `wit-parser` may normalize versions.

## CLI syntax

```bash
spin up \
  --plugin cache=./libcache.so \
  --plugin-config 'cache={"max_entries": 1000}'
```

### `--plugin name=path`

- **name**: Label for log messages and config matching.
- **path**: Path to a shared library (.so, .dylib, .dll).

### `--plugin-config name=json`

Configuration passed to `plugin_init`. If the value starts with `@`, it's treated as a file path:

```bash
--plugin-config 'cache=@/path/to/config.json'
```

Configuration can also come from the `spin.toml` manifest:

```toml
# App-level plugin config
[application]
name = "my-app"

# Per-component plugin access and config
[component.my-handler]
source = "handler.wasm"

[component.my-handler.plugin.cache]
max_entries = 1000
```

Config resolution order:

1. `--plugin-config` CLI values (highest priority)
2. `[component.X.plugin.<name>]` from `spin.toml`
3. Empty config (plugin is initialized with NULL/0)

## Lifecycle

### `Factor::init()`

1. For each `--plugin` argument, load the shared library using `libloading`.
2. Extract required symbols: `plugin_wit_text`, `plugin_dispatch`.
3. Extract optional symbols: `plugin_init`, `plugin_cleanup`.
4. Call `plugin_wit_text()` to get the WIT definition string.
5. Parse WIT, discover exported interfaces and functions.
6. For each exported function, register a forwarding closure in the wasmtime `Linker`.
7. The closure captures a `LazyPluginService` — a `OnceLock` that will be populated later during `configure_app`.

### `Factor::configure_app()`

1. For each loaded plugin:
   - Resolve configuration (CLI → manifest → empty).
   - Call `plugin_init()` with the config bytes (always called, even with empty config).
   - Create a `PluginService` with cached dispatch name resolution.
2. Populate all `LazyPluginService` slots.
3. Build per-component access control from manifest metadata.

### `Factor::prepare()`

Compute allowed interfaces per guest component from `plugin` metadata.

### Guest request handling

When a guest component calls an imported function:

1. The wasmtime linker invokes the forwarding closure.
2. The closure acquires the `PluginService` from the `OnceLock`.
3. Converts `wasmtime::Val` params to `PluginVal`, serializes with postcard.
4. Calls `plugin_dispatch` FFI with the serialized bytes.
5. Deserializes results bytes back to `PluginVal`, converts to `wasmtime::Val`.
6. Writes results into the guest call frame.

## Access control

By default, all guest components can access all plugin interfaces. To restrict access:

```toml
[component.handler]
source = "handler.wasm"

# Only these components can use the cache plugin
[component.handler.plugin.cache]
```

If any `[component.X.plugin.*]` tables exist, only components with explicit entries can access plugins.

# Example: Rust cache plugin

```rust
use postcard::to_allocvec;
use spin_plugin_abi::PluginVal;

static CACHE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

const WIT_TEXT: &str = include_str!("../wit/cache.wit");

#[no_mangle]
pub unsafe extern "C" fn plugin_wit_text() -> *const c_char {
    WIT_TEXT.as_ptr() as *const c_char
}

#[no_mangle]
pub unsafe extern "C" fn plugin_init(
    config_bytes: *const u8,
    config_len: i32,
) -> i32 {
    if config_bytes.is_null() || config_len == 0 {
        // Initialize with defaults
        let mut cache = CACHE.lock().unwrap();
        if cache.is_none() {
            *cache = Some(HashMap::new());
        }
        return 0;
    }

    let bytes = std::slice::from_raw_parts(config_bytes, config_len as usize);
    let config: PluginConfig = match postcard::from_bytes(bytes) {
        Ok(c) => c,
        Err(_) => {
            // Fallback to JSON
            let json_str = std::str::from_utf8(bytes).ok()?;
            serde_json::from_str(json_str).ok()?
        }
    };

    let mut cache = CACHE.lock().unwrap();
    if cache.is_none() {
        *cache = Some(HashMap::with_capacity(config.max_entries));
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn plugin_dispatch(
    func_name: *const c_char,
    params_bytes: *const u8,
    params_len: i32,
    results_buf: *mut u8,
    results_buf_len: i32,
    results_len: *mut i32,
    error_buf: *mut c_char,
    error_buf_len: i32,
) -> i32 {
    let func = CStr::from_ptr(func_name).to_str().ok()?;
    let func_name = func.rsplit_once('#').map(|(_, f)| f).ok()?;

    let params_slice = if params_len > 0 && !params_bytes.is_null() {
        std::slice::from_raw_parts(params_bytes, params_len as usize)
    } else {
        &[]
    };

    let params: Vec<PluginVal> =
        postcard::from_bytes(params_slice).map_err(|e| {
            write_error(error_buf, error_buf_len, &format!("bad params: {}", e));
        })?;

    let result_val: PluginVal = match func_name {
        "get" => handle_get(&params),
        "set" => handle_set(&params),
        "delete" => handle_delete(&params),
        "exists" => handle_exists(&params),
        _ => {
            write_error(error_buf, error_buf_len, &format!("unknown function: {}", func_name));
            return -1;
        }
    };

    // IMPORTANT: use vec![] to serialize as Vec<PluginVal> with length prefix
    let results = to_allocvec(&vec![result_val]).map_err(|e| {
        write_error(error_buf, error_buf_len, &format!("serialize error: {}", e));
    })?;

    if results.len() > results_buf_len as usize {
        write_error(error_buf, error_buf_len, "results buffer too small");
        return -1;
    }

    std::ptr::copy_nonoverlapping(results.as_ptr(), results_buf, results.len());
    std::ptr::write(results_len, results.len() as i32);
    0
}

fn handle_get(params: &[PluginVal]) -> PluginVal {
    let key = match &params[0] {
        PluginVal::String(bytes) => String::from_utf8(bytes.clone()).unwrap(),
        _ => return PluginVal::result_err_string("key must be a string"),
    };

    let cache = CACHE.lock().unwrap();
    match cache.as_ref() {
        Some(map) => match map.get(&key) {
            Some(value) => PluginVal::result_ok_option_string(Some(value)),
            None => PluginVal::result_ok_option_string(None),
        },
        None => PluginVal::result_err_string("cache not initialized"),
    }
}
```

## Example: C cache plugin

A reference C implementation is provided in `examples/c-plugin-cache/src/cache.c`. It implements the same cache interface using manual postcard byte construction:

```c
#include <stdint.h>
#include <string.h>
#include "spin_plugin_abi.h"

static const char* cache_wit_text =
    "package example:cache@0.1.1;\n"
    "interface store {\n"
    "    get: func(key: string) -> result<option<string>, string>;\n"
    "    ...";

static CacheEntry entries[10000];
static int initialized = 0;

const char* plugin_wit_text(void) {
    return cache_wit_text;
}

int plugin_init(const uint8_t* config_bytes, int32_t config_len) {
    // Parse config, set up state
    initialized = 1;
    return 0;
}

int plugin_dispatch(
    const char* func_name,
    const uint8_t* params_bytes,
    int32_t params_len,
    uint8_t* results_buf,
    int32_t results_buf_len,
    int32_t* results_len,
    char* error_buf,
    int32_t error_buf_len
) {
    const char* hash = strrchr(func_name, '#');
    const char* func = hash + 1;

    if (strcmp(func, "get") == 0) {
        char key[256];
        extract_param_string(params_bytes, params_len, 0, key, sizeof(key));

        // Write postcard-serialized result to results_buf
        int written = write_postcard_result(
            results_buf, (int)results_buf_len,
            1, /* is_ok */
            cache_entries[idx].value, /* ok_string */
            0, /* is_option_none */
            NULL /* err */
        );
        *results_len = written;
        return 0;
    }
    // ... handle other functions
}
```

The C example manually constructs postcard bytes because C has no postcard library. The byte layout is documented in the helper functions:

- Vec length prefix: single byte (u128 le128 encoding)
- PluginVal discriminant: single byte (0=Bool, 12=String, 18=Option, 19=Result, 21=Unit)
- Payload bytes: variant-specific

# Implementation

## `crates/plugin-plugin/`

Shared ABI crate, published as `spin-plugin-plugin` (lib name: `spin_plugin_abi`). Contains:

- `PluginVal` enum with serde derives for postcard compatibility
- Helper constructors: `string()`, `result_ok_string()`, `result_ok_bool()`, `result_ok_option_string()`, `result_ok_unit()`, `result_err_string()`

## `crates/factor-plugins/`

Spin factor implementation. Contains:

### `lib.rs`

- `PluginsFactor` — factor struct with plugin paths, configs, loaded plugins, and service slots.
- `PluginLibrary` — loaded shared library with extracted function pointers.
- `PluginVal` ↔ `wasmtime::Val` conversion trait and implementation.
- `parse_plugin_wit()` — WIT parsing, interface discovery, version-aware name construction.
- `register_forwarding_functions()` — linker registration with async closures.
- `val_slice_to_postcard()` / `postcard_to_val_slice()` — serialization helpers.

### `service.rs`

- `PluginService` — cached dispatch name resolution (interface + function → fully qualified name).
- `call_dispatch()` — FFI call wrapper with error context.

### `include/spin_plugin_abi.h`

C header defining the plugin ABI. Copied alongside shared library distributions.

## `examples/rust-plugin-cache/`

Reference Rust implementation:
- `src/lib.rs` — full cache with postcard serialization
- `wit/cache.wit` — WIT definition
- `Cargo.toml` — `cdylib` crate type, `spin-plugin-plugin` dependency

## `examples/c-plugin-cache/`

Reference C implementation:
- `src/cache.c` — manual postcard byte construction
- Builds to `libcache.so` / `libcache.dylib`

## `examples/cache-guest-app/`

Guest application that imports the cache interface:
- Uses `wit_bindgen::generate!` for the cache interface
- HTTP component routing `/set`, `/get`, `/delete`, `/exists` to cache calls
- `spin.toml` manifest with HTTP trigger

# Key design decisions

**Postcard over JSON** — Postcard is more compact (no key strings), faster to parse, and supports zero-copy deserialization. JSON would require string allocation for every parameter and result.

**Shared ABI crate** — The `PluginVal` enum lives in a separate crate (`spin-plugin-plugin`) that both the host and plugins import. This ensures serialization compatibility — if the enum changed, both sides would fail to compile.

**Manual interface name construction** — The WIT interface name is constructed as `namespace:name@version` from the parsed package and interface metadata, rather than using `wit-parser`'s default name resolution. This preserves the exact version string from the WIT source.

**Raw byte buffers** — The C ABI uses raw `uint8_t*` with explicit length, avoiding string allocation. The host provides a results buffer; the plugin writes into it. Neither side frees memory allocated by the other.

**Always call `plugin_init`** — Even when no config is provided, `plugin_init` is called with NULL/0 to allow the plugin to initialize default state. This ensures deterministic initialization.

**Vec serialization, not array** — Plugin results are serialized as `Vec<PluginVal>` (with length prefix), not `[PluginVal; N]` (fixed-size array without length prefix). The host always deserializes as `Vec<PluginVal>`. This is a common pitfall — `postcard::to_allocvec(&[val])` produces array encoding, while `postcard::to_allocvec(&vec![val])` produces Vec encoding with the required length prefix.

**Lazy service population** — Forwarding closures registered during `init()` capture `OnceLock<PluginService>` references that are populated later during `configure_app()`. This decouples linker registration (which happens early) from plugin initialization (which happens during app configuration).

# Future work

- **Rust binding generator** — A procedural macro or build script that generates the `plugin_dispatch` function from WIT, reducing boilerplate for Rust plugin authors.
- **Resource type support** — Extend the serialization to handle component-model resource types (handles, borrows) for plugins that need to manage stateful resources.
- **Plugin registry** — Distribute pre-built plugins via a registry or OCI-compatible storage.
- **Plugin update at runtime** — Hot-swap plugin shared libraries without restarting the Spin process.
- **Structured config schema** — Plugins export a JSON Schema or WIT type describing expected configuration for validation and tooling support.

# Alternatives considered

## Wasm components (SIP 024)

Wasm components are sandbox-safe and portable, but add runtime overhead. C shared libraries are faster and integrate with existing native ecosystems. Both approaches coexist — use Wasm for portability, native for performance.

## JSON serialization

JSON is human-readable and has broader language support. Rejected in favor of postcard for: compactness, speed, zero-copy potential, and the fact that plugin authors typically have postcard available in their build chain.

## gRPC / IPC sidecar

Out-of-process plugins would be sandbox-safe and language-agnostic. Rejected for the initial implementation due to operational complexity, latency, and serialization overhead. In-process native plugins are simpler and faster.

## Compile-time binding generation

A `wit-bindgen`-like tool could generate type-safe Rust bindings for plugin authors. Defered — the dynamic `PluginVal` approach works for the MVP and supports all WIT types without code generation.

## Per-plugin wasmtime Store

Each plugin could run in its own wasmtime Store, providing sandboxing. Rejected for native plugins — the plugin runs as a shared library with full process access, which is appropriate for host-side code.

# Comparing SIP 024 and SIP 025

| Aspect | SIP 024 (Host Components) | SIP 025 (C Plugins) |
|--------|--------------------------|---------------------|
| Format | Wasm component (.wasm) | Shared library (.so/.dylib/.dll) |
| Serialization | wasmtime `Val` (in-process) | Postcard bytes (C FFI) |
| Sandboxing | Wasm sandbox | None (native code) |
| Config interface | `spin:host-component/configure` WIT | `plugin_init()` C function |
| Filesystem | Optional `/data` mount | Full filesystem access |
| Performance | Wasm execution overhead | Native speed |
| Portability | Architecture-independent | Architecture-dependent |
| Use case | Portable, sandboxed services | Performance-critical, native integrations |

Both approaches share the same high-level model: dynamic interface registration, linker forwarding, per-component access control, and CLI-driven loading. The choice depends on whether the priority is portability (SIP 024) or performance (SIP 025).
