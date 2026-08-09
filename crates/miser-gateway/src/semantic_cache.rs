use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct SemanticCache {
    entries: Mutex<Vec<SemanticEntry>>,
    max_entries: usize,
    ttl: Duration,
    similarity_threshold: f32,
}

struct SemanticEntry {
    embedding: Vec<f32>,
    body: bytes::Bytes,
    status: axum::http::StatusCode,
    headers: axum::http::HeaderMap,
    inserted: Instant,
}

impl SemanticCache {
    pub fn new(max_entries: usize, ttl_seconds: u64, similarity_threshold: f32) -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            max_entries,
            ttl: Duration::from_secs(ttl_seconds),
            similarity_threshold,
        }
    }

    pub fn lookup(
        &self,
        embedding: &[f32],
    ) -> Option<(bytes::Bytes, axum::http::StatusCode, axum::http::HeaderMap)> {
        let mut entries = self.entries.lock().ok()?;
        let now = Instant::now();
        entries.retain(|e| now.duration_since(e.inserted) < self.ttl);
        let mut best: Option<(f32, &SemanticEntry)> = None;
        for entry in entries.iter() {
            let sim = cosine_similarity(embedding, &entry.embedding);
            if sim >= self.similarity_threshold && (best.is_none() || sim > best.unwrap().0) {
                best = Some((sim, entry));
            }
        }
        best.map(|(_, entry)| (entry.body.clone(), entry.status, entry.headers.clone()))
    }

    pub fn store(
        &self,
        embedding: Vec<f32>,
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

pub fn embed_prompt(text: &str) -> Vec<f32> {
    let lower = text.to_lowercase();
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty() && s.len() > 2)
        .collect();
    let mut bag: HashMap<String, f32> = HashMap::new();
    for token in &tokens {
        *bag.entry(token.to_string()).or_insert(0.0) += 1.0;
    }
    let total: f32 = bag.values().sum();
    let mut embedding: Vec<f32> = if total > 0.0 {
        bag.values().map(|v| v / total).collect()
    } else {
        vec![0.0; 1]
    };
    let magnitude: f32 = embedding.iter().map(|v| v * v).sum::<f32>().sqrt();
    if magnitude > 0.0 {
        for v in &mut embedding {
            *v /= magnitude;
        }
    }
    embedding
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
