use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A semantically-similar cached response. `similarity` is the embedding
/// cosine; `prompt_text` is the original request text so the Jev judge can
/// validate answer-equivalence before the hit is served.
pub struct SemanticHit {
    pub similarity: f32,
    pub prompt_text: String,
    pub body: bytes::Bytes,
    pub status: axum::http::StatusCode,
    pub headers: axum::http::HeaderMap,
}

pub struct SemanticCache {
    entries: Mutex<Vec<SemanticEntry>>,
    max_entries: usize,
    ttl: Duration,
    candidate_threshold: f32,
}

struct SemanticEntry {
    embedding: Vec<f32>,
    prompt_text: String,
    body: bytes::Bytes,
    status: axum::http::StatusCode,
    headers: axum::http::HeaderMap,
    inserted: Instant,
}

impl SemanticCache {
    pub fn new(max_entries: usize, ttl_seconds: u64, candidate_threshold: f32) -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            max_entries,
            ttl: Duration::from_secs(ttl_seconds),
            candidate_threshold,
        }
    }

    /// Best embedding match above the candidate threshold. The caller must
    /// validate equivalence (Jev) before serving.
    pub fn lookup(&self, embedding: &[f32]) -> Option<SemanticHit> {
        let mut entries = self.entries.lock().ok()?;
        let now = Instant::now();
        entries.retain(|e| now.duration_since(e.inserted) < self.ttl);
        let mut best: Option<(f32, &SemanticEntry)> = None;
        for entry in entries.iter() {
            let sim = cosine_similarity(embedding, &entry.embedding);
            if sim >= self.candidate_threshold && (best.is_none() || sim > best.unwrap().0) {
                best = Some((sim, entry));
            }
        }
        best.map(|(similarity, entry)| SemanticHit {
            similarity,
            prompt_text: entry.prompt_text.clone(),
            body: entry.body.clone(),
            status: entry.status,
            headers: entry.headers.clone(),
        })
    }

    pub fn store(
        &self,
        embedding: Vec<f32>,
        prompt_text: String,
        body: bytes::Bytes,
        status: axum::http::StatusCode,
        headers: axum::http::HeaderMap,
    ) {
        if let Ok(mut entries) = self.entries.lock() {
            if entries.len() >= self.max_entries {
                entries.sort_by_key(|e| e.inserted);
                entries.remove(0);
            }
            entries.push(SemanticEntry {
                embedding,
                prompt_text,
                body,
                status,
                headers,
                inserted: Instant::now(),
            });
        }
    }

    #[allow(dead_code)]
    pub fn stats(&self) -> (usize, usize) {
        let entries = self.entries.lock().map(|e| e.len()).unwrap_or(0);
        (entries, self.max_entries)
    }
}

/// Feature-hashed bag-of-words embedding. Tokens hash into a fixed 512-
/// dimensional space so every prompt lives in the same coordinate system —
/// the previous implementation took `HashMap::values()` in arbitrary order,
/// making cosines between different prompts meaningless noise.
pub fn embed_prompt(text: &str) -> Vec<f32> {
    const DIM: usize = 512;
    let mut bag = vec![0.0f32; DIM];
    let lower = text.to_lowercase();
    for token in lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty() && s.len() > 2)
    {
        let idx = fnv1a(token) as usize % DIM;
        bag[idx] += 1.0;
    }
    let magnitude = bag.iter().map(|v| v * v).sum::<f32>().sqrt();
    if magnitude > 0.0 {
        for v in &mut bag {
            *v /= magnitude;
        }
    }
    bag
}

fn fnv1a(token: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in token.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut mag_a = 0.0;
    let mut mag_b = 0.0;
    let len = a.len().min(b.len());
    for i in 0..len {
        dot += a[i] * b[i];
        mag_a += a[i] * a[i];
        mag_b += b[i] * b[i];
    }
    let denom = mag_a.sqrt() * mag_b.sqrt();
    if denom > 0.0 { dot / denom } else { 0.0 }
}

pub fn request_text_for_embedding(body: &serde_json::Value) -> String {
    if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
        messages
            .iter()
            .filter_map(|msg| {
                msg.get("content")
                    .and_then(|c| c.as_str())
                    .map(|s| s.to_string())
            })
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        body.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cache() -> SemanticCache {
        SemanticCache::new(10, 300, 0.75)
    }

    #[test]
    fn store_then_lookup_returns_prompt_text_and_body() {
        let cache = cache();
        let emb = embed_prompt("fix the login bug in the auth service");
        cache.store(
            emb,
            "fix the login bug in the auth service".into(),
            bytes::Bytes::from_static(b"{}"),
            axum::http::StatusCode::OK,
            axum::http::HeaderMap::new(),
        );
        let hit = cache
            .lookup(&embed_prompt(
                "please fix the login bug in the auth service",
            ))
            .expect("near-duplicate should candidate");
        assert!(hit.similarity >= 0.75);
        assert_eq!(hit.prompt_text, "fix the login bug in the auth service");
    }

    #[test]
    fn dissimilar_prompts_do_not_candidate() {
        let cache = cache();
        cache.store(
            embed_prompt("write a sonnet about autumn leaves"),
            "write a sonnet about autumn leaves".into(),
            bytes::Bytes::from_static(b"{}"),
            axum::http::StatusCode::OK,
            axum::http::HeaderMap::new(),
        );
        assert!(
            cache
                .lookup(&embed_prompt(
                    "explain database transaction isolation levels"
                ))
                .is_none()
        );
    }

    #[test]
    fn entries_expire_after_ttl() {
        let cache = SemanticCache::new(10, 0, 0.75);
        cache.store(
            embed_prompt("hello world example"),
            "hello world example".into(),
            bytes::Bytes::from_static(b"{}"),
            axum::http::StatusCode::OK,
            axum::http::HeaderMap::new(),
        );
        assert!(cache.lookup(&embed_prompt("hello world example")).is_none());
    }

    fn store_entry(
        cache: &SemanticCache,
        embedding: Vec<f32>,
        prompt: &str,
        status: axum::http::StatusCode,
        headers: &axum::http::HeaderMap,
    ) {
        cache.store(
            embedding,
            prompt.to_owned(),
            bytes::Bytes::from(format!("body-of:{prompt}")),
            status,
            headers.clone(),
        );
    }

    #[test]
    fn hit_preserves_body_status_and_headers() {
        let cache = cache();
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "content-type",
            axum::http::HeaderValue::from_static("application/json"),
        );
        let prompt = "explain the borrow checker";
        store_entry(
            &cache,
            embed_prompt(prompt),
            prompt,
            axum::http::StatusCode::OK,
            &headers,
        );
        let hit = cache
            .lookup(&embed_prompt(prompt))
            .expect("identical prompt must hit");
        assert_eq!(
            hit.body,
            bytes::Bytes::from_static(b"body-of:explain the borrow checker")
        );
        assert_eq!(hit.status, axum::http::StatusCode::OK);
        assert_eq!(hit.headers, headers);
        assert_eq!(hit.prompt_text, prompt);
    }

    #[test]
    fn candidate_threshold_boundary_is_inclusive() {
        // cos([1,0], [0.6,0.8]) = 0.6 exactly in f32.
        let emb_a = vec![1.0f32, 0.0];
        let emb_b = vec![0.6f32, 0.8];

        let inclusive = SemanticCache::new(10, 300, 0.6);
        store_entry(
            &inclusive,
            emb_a.clone(),
            "a",
            axum::http::StatusCode::OK,
            &axum::http::HeaderMap::new(),
        );
        let hit = inclusive
            .lookup(&emb_b)
            .expect("similarity == threshold must candidate");
        assert!((hit.similarity - 0.6).abs() < 1e-6);

        let exclusive = SemanticCache::new(10, 300, 0.65);
        store_entry(
            &exclusive,
            emb_a,
            "a",
            axum::http::StatusCode::OK,
            &axum::http::HeaderMap::new(),
        );
        assert!(
            exclusive.lookup(&emb_b).is_none(),
            "similarity below threshold must not candidate"
        );
    }

    #[test]
    fn lookup_returns_the_most_similar_entry() {
        let cache = cache();
        store_entry(
            &cache,
            vec![1.0, 0.0],
            "x-axis prompt",
            axum::http::StatusCode::OK,
            &axum::http::HeaderMap::new(),
        );
        store_entry(
            &cache,
            vec![0.0, 1.0],
            "y-axis prompt",
            axum::http::StatusCode::OK,
            &axum::http::HeaderMap::new(),
        );
        // Query closer to the y-axis entry.
        let hit = cache
            .lookup(&[0.05, 1.0])
            .expect("close match must candidate");
        assert_eq!(hit.prompt_text, "y-axis prompt");
    }

    #[test]
    fn cap_eviction_drops_oldest_stored_entry() {
        let cache = SemanticCache::new(2, 300, 0.75);
        store_entry(
            &cache,
            embed_prompt("first prompt"),
            "first prompt",
            axum::http::StatusCode::OK,
            &axum::http::HeaderMap::new(),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        store_entry(
            &cache,
            embed_prompt("second prompt"),
            "second prompt",
            axum::http::StatusCode::OK,
            &axum::http::HeaderMap::new(),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        store_entry(
            &cache,
            embed_prompt("third prompt"),
            "third prompt",
            axum::http::StatusCode::OK,
            &axum::http::HeaderMap::new(),
        );
        assert!(
            cache.lookup(&embed_prompt("first prompt")).is_none(),
            "oldest entry evicted at cap"
        );
        assert!(cache.lookup(&embed_prompt("second prompt")).is_some());
        assert!(cache.lookup(&embed_prompt("third prompt")).is_some());
        assert_eq!(cache.stats().0, 2);
    }

    #[test]
    fn embed_prompt_is_deterministic_and_case_insensitive() {
        let upper = embed_prompt("Fix the Login Bug");
        let lower = embed_prompt("fix the login bug");
        assert_eq!(upper.len(), 512, "fixed 512-dim feature space");
        assert_eq!(upper, lower, "token hashing must be case-insensitive");
    }

    #[test]
    fn empty_embedding_never_candidates() {
        let cache = cache();
        // A prompt with no tokens (>2 alphanumeric chars) embeds to the zero
        // vector; cosine is defined as 0 there, below any sane threshold.
        store_entry(
            &cache,
            embed_prompt("!! ??"),
            "punctuation only",
            axum::http::StatusCode::OK,
            &axum::http::HeaderMap::new(),
        );
        assert!(cache.lookup(&embed_prompt("!! ??")).is_none());
    }

    #[test]
    fn request_text_joins_message_contents() {
        let body = json!({
            "messages": [
                {"role": "user", "content": "first part"},
                {"role": "assistant", "content": "second part"},
                {"role": "user", "content": {"structured": true}}
            ]
        });
        assert_eq!(
            request_text_for_embedding(&body),
            "first part second part",
            "string contents join; non-string contents are skipped"
        );
        let no_messages = json!({"prompt": "bare"});
        assert_eq!(
            request_text_for_embedding(&no_messages),
            no_messages.to_string(),
            "bodies without a messages array fall back to full serialization"
        );
    }
}
