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
    /// The tier whose route produced this entry. A hit skips classification,
    /// so the handler needs this to enforce a per-key tier allowlist.
    pub tier: String,
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
    tier: String,
    /// The API key this entry belongs to.
    ///
    /// The cache is process-global, and similarity is computed over message
    /// text only, so two different tenants asking *nearly* the same question
    /// matched each other at 0.96 while their system prompts -- the part that
    /// says whose data the answer is about -- were outvoted by the shared
    /// question. One tenant's answer, generated under its own system prompt,
    /// was then served to another. Partitioning by key is what makes a hit mean
    /// "the same tenant asked the same thing again".
    ///
    /// The exact cache does not need this: its key is a hash of the whole
    /// request body, so a hit there is a byte-identical request and the shared
    /// answer is the right answer to the question actually asked.
    tenant: String,
    inserted: Instant,
}

/// Everything a stored entry carries besides its embedding.
///
/// Grouped rather than passed as seven positional arguments, because the two
/// security-relevant fields -- `tier` and `tenant` -- are exactly the kind of
/// thing that gets dropped from an argument list when the next field is added.
/// Both are load-bearing: a hit skips classification, so `tier` is the only
/// thing the handler can gate a per-key allowlist on, and `tenant` is the only
/// thing stopping one key's answer being served to another.
pub struct StoredResponse {
    pub prompt_text: String,
    pub body: bytes::Bytes,
    pub status: axum::http::StatusCode,
    pub headers: axum::http::HeaderMap,
    pub tier: String,
    pub tenant: String,
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

    /// Best embedding match above the candidate threshold, restricted to
    /// `tenant`. The caller must validate equivalence (Jev) before serving.
    pub fn lookup(&self, embedding: &[f32], tenant: &str) -> Option<SemanticHit> {
        let mut entries = self.entries.lock().ok()?;
        let now = Instant::now();
        entries.retain(|e| now.duration_since(e.inserted) < self.ttl);
        let mut best: Option<(f32, &SemanticEntry)> = None;
        for entry in entries.iter() {
            if entry.tenant != tenant {
                continue;
            }
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
            tier: entry.tier.clone(),
        })
    }

    pub fn store(&self, embedding: Vec<f32>, stored: StoredResponse) {
        let StoredResponse {
            prompt_text,
            body,
            status,
            headers,
            tier,
            tenant,
        } = stored;
        if let Ok(mut entries) = self.entries.lock() {
            // A capacity of zero is the documented way to switch this cache
            // off (LLD section 6), so it has to mean "hold nothing" rather
            // than "hold one entry and serve it back". `ResponseCache` gets
            // that for free because it is keyed and a stale key simply never
            // matches again; here a stored entry is reachable by cosine
            // similarity from any near-duplicate prompt, so a lone survivor
            // would keep answering.
            if self.max_entries == 0 {
                return;
            }
            if entries.len() >= self.max_entries {
                entries.sort_by_key(|e| e.inserted);
                // `max_entries >= 1` and `len >= max_entries` together mean
                // the vector is non-empty, but index anyway rather than rely
                // on that: the panic took the whole process down through the
                // poisoned lock, and the invariant is one config edit away.
                if !entries.is_empty() {
                    entries.remove(0);
                }
            }
            entries.push(SemanticEntry {
                embedding,
                prompt_text,
                body,
                status,
                headers,
                tier,
                tenant,
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

/// The text whose embedding decides whether two requests are "the same
/// question".
///
/// Message text alone is not a fingerprint of the request. It used to be the
/// only input, so `max_tokens`, `temperature`, `top_p` and `stop` were
/// invisible to the cache: a request for 8 tokens was served the cached body of
/// an otherwise identical request that was allowed 4096, together with the
/// first request's `usage` and `model` fields. `request_hash` keeps every one of
/// those in its key, so the comment claiming the no-judge fallback "mirrors the
/// exact-match safety bar" was simply false.
///
/// The parameters are appended rather than hashed in separately, because the
/// judge that validates a hit is handed this same text and should see what it is
/// being asked to compare.
pub fn request_text_for_embedding(body: &serde_json::Value) -> String {
    let mut text = match body.get("messages").and_then(|m| m.as_array()) {
        Some(messages) => messages
            .iter()
            .filter_map(|msg| {
                msg.get("content")
                    .and_then(|c| c.as_str())
                    .map(|s| s.to_string())
            })
            .collect::<Vec<_>>()
            .join(" "),
        None => body.to_string(),
    };
    // Only parameters that change the answer. `stream` is excluded because a
    // streamed response is never stored, and `user`/`seed` because the exact
    // cache excludes them too.
    for field in [
        "max_tokens",
        "max_completion_tokens",
        "temperature",
        "top_p",
        "stop",
    ] {
        if let Some(value) = body.get(field) {
            text.push(' ');
            text.push_str(field);
            text.push('=');
            text.push_str(&value.to_string());
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cache() -> SemanticCache {
        SemanticCache::new(10, 300, 0.75)
    }

    /// A stored response with the metadata the handler needs on a hit.
    fn stored(prompt: &str, body: &'static [u8]) -> StoredResponse {
        StoredResponse {
            prompt_text: prompt.to_owned(),
            body: bytes::Bytes::from_static(body),
            status: axum::http::StatusCode::OK,
            headers: axum::http::HeaderMap::new(),
            tier: "trivial".into(),
            tenant: "acme".into(),
        }
    }

    #[test]
    fn store_then_lookup_returns_prompt_text_and_body() {
        let cache = cache();
        let emb = embed_prompt("fix the login bug in the auth service");
        cache.store(emb, stored("fix the login bug in the auth service", b"{}"));
        let hit = cache
            .lookup(
                &embed_prompt("please fix the login bug in the auth service"),
                "acme",
            )
            .expect("near-duplicate should candidate");
        assert!(hit.similarity >= 0.75);
        assert_eq!(hit.prompt_text, "fix the login bug in the auth service");
    }

    #[test]
    fn a_zero_capacity_cache_is_disabled_rather_than_a_one_entry_cache() {
        // LLD section 6 documents `max_entries: 0` as the switch that turns
        // this cache off. It used to be a live panic instead: the eviction
        // branch called `Vec::remove(0)` on an empty vector, and a panic
        // inside the lock poisons it, so the cache was dead for the life of
        // the process and every later store was a silent no-op.
        let cache = SemanticCache::new(0, 300, 0.75);
        let emb = embed_prompt("fix the login bug in the auth service");
        cache.store(
            emb.clone(),
            stored("fix the login bug in the auth service", b"{}"),
        );
        // And it must stay empty rather than retaining exactly one reachable
        // entry, which is what the exact cache's keyed lookup gets away with.
        assert!(cache.lookup(&emb, "acme").is_none());
        assert_eq!(cache.stats().0, 0);
    }

    #[test]
    fn a_one_capacity_cache_keeps_exactly_the_newest_entry() {
        let cache = SemanticCache::new(1, 300, 0.75);
        cache.store(
            embed_prompt("first prompt about database indexes"),
            stored("first prompt about database indexes", b"{\"n\":1}"),
        );
        cache.store(
            embed_prompt("second prompt about database indexes"),
            stored("second prompt about database indexes", b"{\"n\":2}"),
        );
        assert_eq!(cache.stats().0, 1);
        let hit = cache
            .lookup(
                &embed_prompt("second prompt about database indexes"),
                "acme",
            )
            .expect("newest entry is still served");
        assert_eq!(hit.body, bytes::Bytes::from_static(b"{\"n\":2}"));
    }

    #[test]
    fn dissimilar_prompts_do_not_candidate() {
        let cache = cache();
        cache.store(
            embed_prompt("write a sonnet about autumn leaves"),
            stored("write a sonnet about autumn leaves", b"{}"),
        );
        assert!(
            cache
                .lookup(
                    &embed_prompt("explain database transaction isolation levels"),
                    "acme"
                )
                .is_none()
        );
    }

    #[test]
    fn entries_expire_after_ttl() {
        let cache = SemanticCache::new(10, 0, 0.75);
        cache.store(
            embed_prompt("hello world example"),
            stored("hello world example", b"{}"),
        );
        assert!(
            cache
                .lookup(&embed_prompt("hello world example"), "acme")
                .is_none()
        );
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
            StoredResponse {
                prompt_text: prompt.to_owned(),
                body: bytes::Bytes::from(format!("body-of:{prompt}")),
                status,
                headers: headers.clone(),
                tier: "trivial".into(),
                tenant: "acme".into(),
            },
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
            .lookup(&embed_prompt(prompt), "acme")
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
            .lookup(&emb_b, "acme")
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
            exclusive.lookup(&emb_b, "acme").is_none(),
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
            .lookup(&[0.05, 1.0], "acme")
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
            cache
                .lookup(&embed_prompt("first prompt"), "acme")
                .is_none(),
            "oldest entry evicted at cap"
        );
        assert!(
            cache
                .lookup(&embed_prompt("second prompt"), "acme")
                .is_some()
        );
        assert!(
            cache
                .lookup(&embed_prompt("third prompt"), "acme")
                .is_some()
        );
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
        assert!(cache.lookup(&embed_prompt("!! ??"), "acme").is_none());
    }

    /// The embedding text is a fingerprint of the request, not just of its
    /// prose. Message text alone made `max_tokens`, `temperature`, `top_p` and
    /// `stop` invisible to the cache, so a request capped at 8 tokens was served
    /// the cached body of an otherwise identical request allowed 4096 --
    /// including the first request's `usage` and `model` fields. The claim that
    /// the no-judge fallback "mirrors the exact-match safety bar" was false:
    /// `request_hash` keeps all of these in its key.
    #[test]
    fn generation_parameters_are_part_of_the_embedding_text() {
        let base = json!({
            "messages": [{"role": "user", "content": "list the first three prime numbers"}]
        });
        let mut capped = base.clone();
        capped["max_tokens"] = json!(8);
        let mut roomy = base.clone();
        roomy["max_tokens"] = json!(4096);
        let mut hot = base.clone();
        hot["temperature"] = json!(0.9);

        let text = request_text_for_embedding(&base);
        for (label, body) in [
            ("max_tokens=8", &capped),
            ("max_tokens=4096", &roomy),
            ("temperature=0.9", &hot),
        ] {
            assert_ne!(
                request_text_for_embedding(body),
                text,
                "{label} must change the embedding text, or a cached answer \
                 crosses a generation-parameter boundary"
            );
        }
        // ...and they must actually separate the two in cosine space, not just
        // add a token that the shared prose outvotes.
        let a = embed_prompt(&request_text_for_embedding(&capped));
        let b = embed_prompt(&request_text_for_embedding(&roomy));
        let short_prompt = "list the first three prime numbers";
        let plain_a = embed_prompt(short_prompt);
        let plain_b = embed_prompt(short_prompt);
        assert!(
            cosine_similarity(&a, &b) < cosine_similarity(&plain_a, &plain_b),
            "a max_tokens difference must reduce similarity"
        );
    }

    /// Two tenants must never share a semantic entry, however similar their
    /// prompts. The cache is process-global, so without the partition one key's
    /// answer -- generated under its own system prompt -- is served to another.
    #[test]
    fn a_lookup_never_crosses_tenants() {
        let cache = cache();
        let prompt = "summarise the quarterly reconciliation discrepancies across the three \
                      payment processors and explain which are timing differences rather than \
                      genuinely lost transactions, and what evidence to pull before deciding";
        cache.store(
            embed_prompt(&format!("You are AcmeCorp billing. {prompt} ")),
            StoredResponse {
                prompt_text: prompt.to_owned(),
                body: bytes::Bytes::from_static(b"{\"tenant\":\"acme\"}"),
                status: axum::http::StatusCode::OK,
                headers: axum::http::HeaderMap::new(),
                tier: "trivial".into(),
                tenant: "key_acme".into(),
            },
        );
        let other = embed_prompt(&format!("Be brief. {prompt} "));
        assert!(
            cosine_similarity(
                &other,
                &embed_prompt(&format!("You are AcmeCorp billing. {prompt} "))
            ) > 0.92,
            "test premise: the two prompts must be similar enough that only the \
             tenant boundary can separate them"
        );
        assert!(
            cache.lookup(&other, "key_globex").is_none(),
            "another tenant's entry must be invisible"
        );
        assert!(
            cache.lookup(&other, "key_acme").is_some(),
            "the owning tenant must still hit its own entry"
        );
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
