use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

wit_bindgen::generate!({
    world: "cache-provider",
    path: "wit",
    generate_all,
});

struct CacheProvider;
export!(CacheProvider);

/// Default maximum total bytes per store before eviction kicks in.
const DEFAULT_MAX_STORE_BYTES: usize = 64 * 1024 * 1024; // 64 MiB

/// Configurable max bytes, settable via set-max-bytes.
static MAX_BYTES: Mutex<usize> = Mutex::new(DEFAULT_MAX_STORE_BYTES);

/// Global monotonic counter for LRU ordering.
static ACCESS_COUNTER: Mutex<u64> = Mutex::new(0);

fn next_access_id() -> u64 {
    let mut c = ACCESS_COUNTER.lock().unwrap();
    *c += 1;
    *c
}

struct CacheEntry {
    value: Vec<u8>,
    expires_at: Option<u64>, // ms since epoch, None = no expiry
    access_id: u64,          // monotonic, higher = more recently used
}

impl CacheEntry {
    fn size(&self) -> usize {
        self.value.len()
    }

    fn is_expired(&self, now: u64) -> bool {
        self.expires_at.map_or(false, |exp| now > exp)
    }
}

/// Per-store state: entries plus a running total of value bytes.
struct CacheStore {
    entries: HashMap<String, CacheEntry>,
    total_bytes: usize,
}

impl CacheStore {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            total_bytes: 0,
        }
    }

    /// Remove all expired entries, returning how many were evicted.
    fn evict_expired(&mut self, now: u64) -> usize {
        let expired_keys: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, e)| e.is_expired(now))
            .map(|(k, _)| k.clone())
            .collect();
        let count = expired_keys.len();
        for k in expired_keys {
            if let Some(entry) = self.entries.remove(&k) {
                self.total_bytes -= entry.size();
            }
        }
        count
    }

    /// Evict least-recently-used entries until total_bytes <= limit.
    fn evict_lru(&mut self, limit: usize) {
        if self.total_bytes <= limit {
            return;
        }

        // Build a sorted map of (access_id -> key) so we evict oldest first.
        let mut by_access: BTreeMap<u64, String> = BTreeMap::new();
        for (k, e) in &self.entries {
            by_access.insert(e.access_id, k.clone());
        }

        for (_access_id, key) in by_access {
            if self.total_bytes <= limit {
                break;
            }
            if let Some(entry) = self.entries.remove(&key) {
                self.total_bytes -= entry.size();
            }
        }
    }

    /// Insert or update an entry, running eviction as needed.
    fn put(&mut self, key: String, value: Vec<u8>, ttl_ms: Option<u64>, now: u64) {
        // Remove old entry for this key if present.
        if let Some(old) = self.entries.remove(&key) {
            self.total_bytes -= old.size();
        }

        let max_bytes = *MAX_BYTES.lock().unwrap();
        let expires_at = ttl_ms.map(|ttl| now + ttl);
        let entry = CacheEntry {
            value,
            expires_at,
            access_id: next_access_id(),
        };
        let entry_size = entry.size();

        // Make room: first try expired, then LRU.
        if self.total_bytes + entry_size > max_bytes {
            self.evict_expired(now);
        }
        if self.total_bytes + entry_size > max_bytes {
            let target = max_bytes.saturating_sub(entry_size);
            self.evict_lru(target);
        }

        self.total_bytes += entry_size;
        self.entries.insert(key, entry);
    }

    fn get_val(&mut self, key: &str, now: u64) -> Option<Vec<u8>> {
        // Check & lazily remove if expired.
        if let Some(entry) = self.entries.get(key) {
            if entry.is_expired(now) {
                let entry = self.entries.remove(key).unwrap();
                self.total_bytes -= entry.size();
                return None;
            }
        } else {
            return None;
        }

        // Touch: update access_id for LRU.
        let entry = self.entries.get_mut(key).unwrap();
        entry.access_id = next_access_id();
        Some(entry.value.clone())
    }

    fn delete_key(&mut self, key: &str) -> bool {
        if let Some(entry) = self.entries.remove(key) {
            self.total_bytes -= entry.size();
            true
        } else {
            false
        }
    }

    fn key_exists(&mut self, key: &str, now: u64) -> bool {
        if let Some(entry) = self.entries.get(key) {
            if entry.is_expired(now) {
                let entry = self.entries.remove(key).unwrap();
                self.total_bytes -= entry.size();
                return false;
            }
            true
        } else {
            false
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64
}

static STORES: Mutex<Option<HashMap<String, CacheStore>>> = Mutex::new(None);

fn with_store<R>(name: &str, f: impl FnOnce(&mut CacheStore) -> R) -> R {
    let mut guard = STORES.lock().unwrap();
    let stores = guard.get_or_insert_with(HashMap::new);
    let store = stores.entry(name.to_string()).or_insert_with(CacheStore::new);
    f(store)
}

impl exports::example::cache::store::Guest for CacheProvider {
    fn get(
        store_name: String,
        key: String,
    ) -> Result<Option<Vec<u8>>, exports::example::cache::store::Error> {
        let now = now_ms();
        Ok(with_store(&store_name, |s| s.get_val(&key, now)))
    }

    fn set(
        store_name: String,
        key: String,
        value: Vec<u8>,
        ttl_ms: Option<u64>,
    ) -> Result<(), exports::example::cache::store::Error> {
        let now = now_ms();
        with_store(&store_name, |s| s.put(key, value, ttl_ms, now));
        Ok(())
    }

    fn delete(
        store_name: String,
        key: String,
    ) -> Result<(), exports::example::cache::store::Error> {
        with_store(&store_name, |s| {
            s.delete_key(&key);
        });
        Ok(())
    }

    fn exists(
        store_name: String,
        key: String,
    ) -> Result<bool, exports::example::cache::store::Error> {
        let now = now_ms();
        Ok(with_store(&store_name, |s| s.key_exists(&key, now)))
    }
}

impl exports::spin::host_component::configure::Guest for CacheProvider {
    fn get_key() -> String {
        "cache".to_string()
    }

    fn init(config: String) -> Result<(), String> {
        if let Some(max_bytes) = parse_u64_field(&config, "max_bytes") {
            *MAX_BYTES.lock().unwrap() = max_bytes as usize;
        }
        Ok(())
    }
}

/// Minimal JSON parser: extract a u64 value for a given field name from a JSON object.
fn parse_u64_field(json: &str, field: &str) -> Option<u64> {
    // Look for "field" : <number>
    let pattern = format!("\"{}\"", field);
    let idx = json.find(&pattern)?;
    let after_key = &json[idx + pattern.len()..];
    // Skip whitespace and colon
    let after_colon = after_key.trim_start().strip_prefix(':')?;
    let trimmed = after_colon.trim_start();
    // Parse the number
    let end = trimmed.find(|c: char| !c.is_ascii_digit()).unwrap_or(trimmed.len());
    trimmed[..end].parse().ok()
}
