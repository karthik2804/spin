use std::collections::HashMap;
use std::ffi::{c_char, CStr};
use std::sync::Mutex;

use postcard::to_allocvec;
use spin_plugin_abi::PluginVal;

/// Serializable plugin configuration
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
struct PluginConfig {
    max_entries: Option<usize>,
}

/// Cache entry
#[derive(Clone)]
struct Entry {
    value: String,
}

/// Global cache state
static CACHE: Mutex<Option<CacheState>> = Mutex::new(None);

struct CacheState {
    entries: HashMap<String, Entry>,
    max_entries: usize,
}

impl CacheState {
    fn new(max_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            max_entries,
        }
    }
}

/// WIT definition for the cache plugin
const WIT_TEXT: &str = include_str!("../wit/cache.wit");

/// Returns the WIT definition as a C string.
#[no_mangle]
pub unsafe extern "C" fn plugin_wit_text() -> *const c_char {
    WIT_TEXT.as_ptr() as *const c_char
}

/// Initializes the plugin with postcard-serialized configuration.
#[no_mangle]
pub unsafe extern "C" fn plugin_init(config_bytes: *const u8, config_len: i32) -> i32 {
    if config_bytes.is_null() || config_len == 0 {
        let mut cache = CACHE.lock().unwrap();
        if cache.is_none() {
            *cache = Some(CacheState::new(10000));
        }
        return 0;
    }

    let bytes = std::slice::from_raw_parts(config_bytes, config_len as usize);

    // Try to deserialize as PluginConfig (postcard)
    let config: PluginConfig = match postcard::from_bytes(bytes) {
        Ok(c) => c,
        Err(_) => {
            // Fallback: treat as JSON string
            let json_str = match std::str::from_utf8(bytes) {
                Ok(s) => s,
                Err(_) => return 1,
            };
            match serde_json::from_str(json_str) {
                Ok(c) => c,
                Err(_) => return 1,
            }
        }
    };

    let max_entries = config.max_entries.unwrap_or(10000);
    let mut cache = CACHE.lock().unwrap();
    if cache.is_none() {
        *cache = Some(CacheState::new(max_entries));
    }
    0
}

/// Cleans up plugin resources.
#[no_mangle]
pub unsafe extern "C" fn plugin_cleanup() {
    let mut cache = CACHE.lock().unwrap();
    *cache = None;
}

/// Dispatches a function call to the plugin.
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
    // Helper to write error
    let write_error = |msg: &str| {
        if !error_buf.is_null() && error_buf_len > 0 {
            let len = msg.len().min(error_buf_len as usize - 1);
            std::ptr::copy_nonoverlapping(msg.as_ptr(), error_buf.cast::<u8>(), len);
            *error_buf.add(len).cast::<u8>() = 0;
        }
        -1
    };

    // Get function name
    let func_name = match CStr::from_ptr(func_name).to_str() {
        Ok(s) => s,
        Err(_) => return write_error("invalid func_name"),
    };

    // Parse function name: package:interface@version#function
    let func = match func_name.rsplit_once('#') {
        Some((_, f)) => f,
        None => return write_error(&format!("invalid function name: {}", func_name)),
    };

    // Deserialize params
    let params_slice = if params_len > 0 && !params_bytes.is_null() {
        std::slice::from_raw_parts(params_bytes, params_len as usize)
    } else {
        &[]
    };

    let params: Vec<PluginVal> = match postcard::from_bytes(params_slice) {
        Ok(p) => p,
        Err(e) => return write_error(&format!("failed to deserialize params: {}", e)),
    };

    // Execute function
    let result_val: PluginVal = match func {
        "get" => handle_get(&params),
        "set" => handle_set(&params),
        "delete" => handle_delete(&params),
        "exists" => handle_exists(&params),
        _ => return write_error(&format!("unknown function: {}", func)),
    };

    // Serialize result
    let results = match to_allocvec(&vec![result_val]) {
        Ok(r) => r,
        Err(e) => return write_error(&format!("failed to serialize result: {}", e)),
    };

    // Write results to buffer
    if results.len() > results_buf_len as usize {
        return write_error("results buffer too small");
    }
    if !results_buf.is_null() {
        std::ptr::copy_nonoverlapping(results.as_ptr(), results_buf, results.len());
    }
    if !results_len.is_null() {
        std::ptr::write(results_len, results.len() as i32);
    }

    0
}

fn handle_get(params: &[PluginVal]) -> PluginVal {
    if params.is_empty() {
        return PluginVal::result_err_string("missing key parameter");
    }

    let key = match &params[0] {
        PluginVal::String(bytes) => match String::from_utf8(bytes.clone()) {
            Ok(s) => s,
            Err(_) => return PluginVal::result_err_string("invalid key encoding"),
        },
        _ => return PluginVal::result_err_string("key must be a string"),
    };

    let cache = CACHE.lock().unwrap();
    let cache = match cache.as_ref() {
        Some(c) => c,
        None => return PluginVal::result_err_string("cache not initialized"),
    };

    match cache.entries.get(&key) {
        Some(entry) => PluginVal::result_ok_option_string(Some(&entry.value)),
        None => PluginVal::result_ok_option_string(None),
    }
}

fn handle_set(params: &[PluginVal]) -> PluginVal {
    if params.len() < 2 {
        return PluginVal::result_err_string("missing key or value parameter");
    }

    let key = match &params[0] {
        PluginVal::String(bytes) => match String::from_utf8(bytes.clone()) {
            Ok(s) => s,
            Err(_) => return PluginVal::result_err_string("invalid key encoding"),
        },
        _ => return PluginVal::result_err_string("key must be a string"),
    };

    let value = match &params[1] {
        PluginVal::String(bytes) => match String::from_utf8(bytes.clone()) {
            Ok(s) => s,
            Err(_) => return PluginVal::result_err_string("invalid value encoding"),
        },
        _ => return PluginVal::result_err_string("value must be a string"),
    };

    let mut cache = CACHE.lock().unwrap();
    let cache = match cache.as_mut() {
        Some(c) => c,
        None => return PluginVal::result_err_string("cache not initialized"),
    };

    if cache.entries.len() >= cache.max_entries && !cache.entries.contains_key(&key) {
        return PluginVal::result_err_string(&format!(
            "cache full (max: {} entries)",
            cache.max_entries
        ));
    }

    cache.entries.insert(
        key,
        Entry {
            value: value.clone(),
        },
    );

    PluginVal::result_ok_unit()
}

fn handle_delete(params: &[PluginVal]) -> PluginVal {
    if params.is_empty() {
        return PluginVal::result_err_string("missing key parameter");
    }

    let key = match &params[0] {
        PluginVal::String(bytes) => match String::from_utf8(bytes.clone()) {
            Ok(s) => s,
            Err(_) => return PluginVal::result_err_string("invalid key encoding"),
        },
        _ => return PluginVal::result_err_string("key must be a string"),
    };

    let mut cache = CACHE.lock().unwrap();
    let cache = match cache.as_mut() {
        Some(c) => c,
        None => return PluginVal::result_err_string("cache not initialized"),
    };

    cache.entries.remove(&key);
    PluginVal::result_ok_unit()
}

fn handle_exists(params: &[PluginVal]) -> PluginVal {
    if params.is_empty() {
        return PluginVal::result_err_string("missing key parameter");
    }

    let key = match &params[0] {
        PluginVal::String(bytes) => match String::from_utf8(bytes.clone()) {
            Ok(s) => s,
            Err(_) => return PluginVal::result_err_string("invalid key encoding"),
        },
        _ => return PluginVal::result_err_string("key must be a string"),
    };

    let cache = CACHE.lock().unwrap();
    let cache = match cache.as_ref() {
        Some(c) => c,
        None => return PluginVal::result_err_string("cache not initialized"),
    };

    PluginVal::result_ok_bool(cache.entries.contains_key(&key))
}
