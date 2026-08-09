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
