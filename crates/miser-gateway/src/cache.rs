use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct ResponseCache {
    entries: Mutex<HashMap<u64, CacheEntry>>,
    max_entries: usize,
    ttl: Duration,
}

struct CacheEntry {
    body: bytes::Bytes,
    status: axum::http::StatusCode,
    headers: axum::http::HeaderMap,
    inserted: Instant,
}

impl ResponseCache {
    pub fn new(max_entries: usize, ttl_seconds: u64) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            max_entries,
            ttl: Duration::from_secs(ttl_seconds),
        }
    }

    pub fn get(
        &self,
        key: u64,
    ) -> Option<(bytes::Bytes, axum::http::StatusCode, axum::http::HeaderMap)> {
        let mut entries = self.entries.lock().ok()?;
        if let Some(entry) = entries.get(&key) {
            if entry.inserted.elapsed() < self.ttl {
                return Some((entry.body.clone(), entry.status, entry.headers.clone()));
            }
            entries.remove(&key);
        }
        None
    }

    pub fn store(
        &self,
        key: u64,
        body: bytes::Bytes,
        status: axum::http::StatusCode,
        headers: axum::http::HeaderMap,
    ) {
        if let Ok(mut entries) = self.entries.lock() {
            if entries.len() >= self.max_entries {
                if let Some(&oldest_key) = entries
                    .iter()
                    .min_by_key(|(_, e)| e.inserted)
                    .map(|(k, _)| k)
                {
                    entries.remove(&oldest_key);
                }
            }
            entries.insert(
                key,
                CacheEntry {
                    body,
                    status,
                    headers,
                    inserted: Instant::now(),
                },
            );
        }
    }

    #[allow(dead_code)]
    pub fn stats(&self) -> (usize, usize) {
        let entries = self.entries.lock().map(|e| e.len()).unwrap_or(0);
        (entries, self.max_entries)
    }
}

pub fn request_hash(body: &serde_json::Value) -> u64 {
    let normalized = if let serde_json::Value::Object(map) = body {
        let mut filtered = serde_json::Map::new();
        for (key, value) in map {
            if key != "model" && key != "user" && key != "seed" {
                filtered.insert(key.clone(), value.clone());
            }
        }
        serde_json::Value::Object(filtered)
    } else {
        body.clone()
    };
    let text = serde_json::to_string(&normalized).unwrap_or_default();
    let mut hash: u64 = 14695981039346656037;
    for byte in text.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store_all(cache: &ResponseCache, entries: &[(u64, &str)]) {
        for (i, (key, body)) in entries.iter().enumerate() {
            // Separate insertion timestamps so "oldest" eviction is
            // deterministic even at coarse timer resolution.
            std::thread::sleep(Duration::from_millis(2));
            cache.store(
                *key,
                bytes::Bytes::from(format!("{body}{i}")),
                axum::http::StatusCode::OK,
                axum::http::HeaderMap::new(),
            );
        }
    }

    #[test]
    fn get_misses_on_unknown_key() {
        let cache = ResponseCache::new(10, 60);
        let key = request_hash(&json!({"messages": [{"role": "user", "content": "hi"}]}));
        assert!(cache.get(key).is_none(), "empty cache must miss");
    }

    #[test]
    fn store_then_get_round_trips_body_status_headers() {
        let cache = ResponseCache::new(10, 60);
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "content-type",
            axum::http::HeaderValue::from_static("application/json"),
        );
        let key = request_hash(&json!({"messages": [{"role": "user", "content": "hi"}]}));
        cache.store(
            key,
            bytes::Bytes::from_static(b"{\"answer\":42}"),
            axum::http::StatusCode::OK,
            headers.clone(),
        );
        let (body, status, got_headers) = cache.get(key).expect("just-stored entry hits");
        assert_eq!(body, bytes::Bytes::from_static(b"{\"answer\":42}"));
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(got_headers, headers, "headers must survive the round trip");
    }

    #[test]
    fn entries_expire_after_ttl() {
        let cache = ResponseCache::new(10, 0);
        let key = request_hash(&json!({"messages": []}));
        cache.store(
            key,
            bytes::Bytes::from_static(b"{}"),
            axum::http::StatusCode::OK,
            axum::http::HeaderMap::new(),
        );
        assert!(cache.get(key).is_none(), "ttl=0 entries must not be served");
        assert!(
            cache.get(key).is_none(),
            "expired entry is dropped, not left to hit later"
        );
    }

    #[test]
    fn capacity_eviction_drops_oldest_inserted_entry() {
        let cache = ResponseCache::new(2, 60);
        let k1 = request_hash(&json!({"n": 1}));
        let k2 = request_hash(&json!({"n": 2}));
        let k3 = request_hash(&json!({"n": 3}));
        store_all(&cache, &[(k1, "one"), (k2, "two"), (k3, "three")]);
        assert!(
            cache.get(k1).is_none(),
            "oldest-inserted entry evicted at cap"
        );
        assert!(cache.get(k2).is_some());
        assert!(cache.get(k3).is_some());
        assert_eq!(cache.stats(), (2, 2), "cap enforced, not exceeded");
    }

    #[test]
    fn re_store_overwrites_entry_without_growing() {
        let cache = ResponseCache::new(10, 60);
        let key = request_hash(&json!({"messages": []}));
        cache.store(
            key,
            bytes::Bytes::from_static(b"old"),
            axum::http::StatusCode::OK,
            axum::http::HeaderMap::new(),
        );
        cache.store(
            key,
            bytes::Bytes::from_static(b"new"),
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            axum::http::HeaderMap::new(),
        );
        let (body, status, _) = cache.get(key).unwrap();
        assert_eq!(body, bytes::Bytes::from_static(b"new"));
        assert_eq!(status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(cache.stats().0, 1, "overwrite must not add a second entry");
    }

    #[test]
    fn request_hash_ignores_model_user_and_seed() {
        let base = json!({
            "model": "a", "user": "u", "seed": 1,
            "messages": [{"role": "user", "content": "hi"}],
            "temperature": 0.5
        });
        // Same routing-relevant body under different model/user/seed and
        // different key order in the JSON source.
        let variant = json!({
            "temperature": 0.5,
            "messages": [{"role": "user", "content": "hi"}],
            "model": "b", "user": "v", "seed": 2
        });
        assert_eq!(
            request_hash(&base),
            request_hash(&variant),
            "model/user/seed must not change the cache key"
        );
        let different = json!({
            "model": "a", "user": "u", "seed": 1,
            "messages": [{"role": "user", "content": "bye"}],
            "temperature": 0.5
        });
        assert_ne!(request_hash(&base), request_hash(&different));
    }

    #[test]
    fn request_hash_is_stable_for_non_object_bodies() {
        // Non-object payloads bypass field filtering but still hash stably.
        let array = json!([1, 2, 3]);
        assert_eq!(request_hash(&array), request_hash(&json!([1, 2, 3])));
        assert_ne!(request_hash(&array), request_hash(&json!([1, 2])));
    }
}
