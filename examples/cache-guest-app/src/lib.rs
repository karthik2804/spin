// Generate bindings for the cache interface provided by the plugin.
wit_bindgen::generate!({
    world: "cache-client",
    path: "cache-wit",
});

use example::cache::store as cache;
use spin_sdk::http::{IntoResponse, Response};
use spin_sdk::http_component;

/// HTTP handler that routes requests to cache operations.
#[http_component]
fn handle(req: http::Request<()>) -> anyhow::Result<impl IntoResponse> {
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();

    let body = match path.as_str() {
        "/set" => handle_set(&query),
        "/get" => handle_get(&query),
        "/delete" => handle_delete(&query),
        "/exists" => handle_exists(&query),
        _ => handle_index(),
    };

    Ok(Response::new(200, body))
}

fn parse_query_param(query: &str, key: &str) -> Option<String> {
    let prefix = format!("{}=", key);
    query
        .split('&')
        .find(|p| p.starts_with(&prefix))
        .and_then(|p| p.strip_prefix(&prefix))
        .map(urldecode)
}

fn urldecode(s: &str) -> String {
    s.replace("+", " ").replace("%20", " ")
}

fn handle_index() -> String {
    "Cache Guest App\n\
     /set?key=foo&value=bar - Set a value\n\
     /get?key=foo - Get a value\n\
     /delete?key=foo - Delete a value\n\
     /exists?key=foo - Check if key exists\n"
        .to_string()
}

fn handle_set(query: &str) -> String {
    let key = parse_query_param(query, "key").unwrap_or_else(|| "greeting".to_string());
    let value =
        parse_query_param(query, "value").unwrap_or_else(|| "Hello from plugin cache!".to_string());

    match cache::set(&key, &value) {
        Ok(()) => format!("OK: set '{}' = '{}'", key, value),
        Err(e) => format!("Error setting '{}': {}", key, e),
    }
}

fn handle_get(query: &str) -> String {
    let key = parse_query_param(query, "key").unwrap_or_else(|| "greeting".to_string());

    match cache::get(&key) {
        Ok(Some(value)) => format!("Found '{}': {}", key, value),
        Ok(None) => format!("Key '{}' not found", key),
        Err(e) => format!("Error getting '{}': {}", key, e),
    }
}

fn handle_delete(query: &str) -> String {
    let key = parse_query_param(query, "key").unwrap_or_else(|| "greeting".to_string());

    match cache::delete(&key) {
        Ok(()) => format!("Deleted '{}'", key),
        Err(e) => format!("Error deleting '{}': {}", key, e),
    }
}

fn handle_exists(query: &str) -> String {
    let key = parse_query_param(query, "key").unwrap_or_else(|| "greeting".to_string());

    match cache::exists(&key) {
        Ok(true) => format!("Key '{}' exists", key),
        Ok(false) => format!("Key '{}' does not exist", key),
        Err(e) => format!("Error checking '{}': {}", key, e),
    }
}
