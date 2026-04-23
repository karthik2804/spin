// Generate bindings only for the cache host component interface.
// HTTP is handled by spin-sdk's #[http_component] macro.
wit_bindgen::generate!({
    world: "cache-client",
    path: "cache-wit",
});

use example::cache::store as cache;
use spin_sdk::http::{IntoResponse, Response};
use spin_sdk::http_component;

/// HTTP handler — routes requests to test functions.
#[http_component]
fn handle(req: http::Request<()>) -> anyhow::Result<impl IntoResponse> {
    let path = req.uri().path();
    let body = match path {
        "/test/cache" | "/test/all" => test_cache(),
        _ => format!(
            "Host Components Test Guest\n\
             Routes:\n  \
             /test/cache  - cache provider tests\n  \
             /test/all    - run all tests\n"
        ),
    };
    Ok(Response::new(200, body))
}

fn test_cache() -> String {
    let mut out = String::new();

    // 1. set + get
    match cache::set("test-store", "greeting", b"Hello, World!", None) {
        Ok(()) => out.push_str("PASS: set greeting\n"),
        Err(e) => out.push_str(&format!("FAIL: set greeting: {:?}\n", e)),
    }
    match cache::get("test-store", "greeting") {
        Ok(Some(val)) => {
            let s = String::from_utf8_lossy(&val);
            if s == "Hello, World!" {
                out.push_str("PASS: get greeting = \"Hello, World!\"\n");
            } else {
                out.push_str(&format!("FAIL: get greeting = {:?} (expected Hello, World!)\n", s));
            }
        }
        Ok(None) => out.push_str("FAIL: get greeting returned None\n"),
        Err(e) => out.push_str(&format!("FAIL: get greeting: {:?}\n", e)),
    }

    // 2. exists
    match cache::exists("test-store", "greeting") {
        Ok(true) => out.push_str("PASS: exists greeting = true\n"),
        Ok(false) => out.push_str("FAIL: exists greeting = false\n"),
        Err(e) => out.push_str(&format!("FAIL: exists greeting: {:?}\n", e)),
    }

    // 3. delete + verify gone
    match cache::delete("test-store", "greeting") {
        Ok(()) => out.push_str("PASS: delete greeting\n"),
        Err(e) => out.push_str(&format!("FAIL: delete greeting: {:?}\n", e)),
    }
    match cache::exists("test-store", "greeting") {
        Ok(false) => out.push_str("PASS: exists greeting after delete = false\n"),
        Ok(true) => out.push_str("FAIL: exists greeting after delete = true\n"),
        Err(e) => out.push_str(&format!("FAIL: exists after delete: {:?}\n", e)),
    }

    // 4. set with TTL (very short), then get immediately should still work
    match cache::set("test-store", "ephemeral", b"temporary", Some(5000)) {
        Ok(()) => out.push_str("PASS: set ephemeral with 5s TTL\n"),
        Err(e) => out.push_str(&format!("FAIL: set ephemeral: {:?}\n", e)),
    }
    match cache::get("test-store", "ephemeral") {
        Ok(Some(val)) if val == b"temporary" => {
            out.push_str("PASS: get ephemeral within TTL\n");
        }
        other => out.push_str(&format!("FAIL: get ephemeral: {:?}\n", other)),
    }

    // 5. multiple keys in same store (state persistence)
    for i in 0..5 {
        let key = format!("counter-{}", i);
        let val = format!("{}", i * 10);
        cache::set("multi-store", &key, val.as_bytes(), None).unwrap();
    }
    let mut multi_ok = true;
    for i in 0..5 {
        let key = format!("counter-{}", i);
        let expected = format!("{}", i * 10);
        match cache::get("multi-store", &key) {
            Ok(Some(val)) if val == expected.as_bytes() => {}
            other => {
                out.push_str(&format!("FAIL: multi key {} = {:?}\n", key, other));
                multi_ok = false;
            }
        }
    }
    if multi_ok {
        out.push_str("PASS: 5 keys persisted across calls\n");
    }

    // 6. overwrite
    cache::set("test-store", "mutable", b"v1", None).unwrap();
    cache::set("test-store", "mutable", b"v2", None).unwrap();
    match cache::get("test-store", "mutable") {
        Ok(Some(val)) if val == b"v2" => out.push_str("PASS: overwrite key\n"),
        other => out.push_str(&format!("FAIL: overwrite key: {:?}\n", other)),
    }

    // 7. Verify operator-configured max_bytes is in effect.
    // With max_bytes=32 (from [host_component.cache] in spin.toml),
    // a 20-byte value should cause eviction of a prior 20-byte value.
    cache::set("size-store", "big1", &[0u8; 20], None).unwrap();
    cache::set("size-store", "big2", &[0u8; 20], None).unwrap();
    match cache::exists("size-store", "big1") {
        Ok(false) => out.push_str("PASS: LRU eviction with operator-configured max_bytes\n"),
        Ok(true) => out.push_str("FAIL: big1 should have been evicted (is max_bytes configured?)\n"),
        Err(e) => out.push_str(&format!("FAIL: exists big1: {:?}\n", e)),
    }
    match cache::exists("size-store", "big2") {
        Ok(true) => out.push_str("PASS: big2 survived eviction\n"),
        Ok(false) => out.push_str("FAIL: big2 should exist\n"),
        Err(e) => out.push_str(&format!("FAIL: exists big2: {:?}\n", e)),
    }

    out
}
