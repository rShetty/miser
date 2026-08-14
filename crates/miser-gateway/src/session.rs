use miser_types::{ChatCompletionRequest, ComplexityTier, MessageContent};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct SessionTracker {
    sessions: Mutex<HashMap<String, (ComplexityTier, Instant)>>,
    max_entries: usize,
    ttl: Duration,
}

impl SessionTracker {
    pub fn new(max_entries: usize, ttl_seconds: u64) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            max_entries,
            ttl: Duration::from_secs(ttl_seconds),
        }
    }

    pub fn get(&self, key: &str) -> Option<ComplexityTier> {
        let mut sessions = self.sessions.lock().ok()?;
        if let Some((tier, instant)) = sessions.get(key) {
            if instant.elapsed() < self.ttl {
                return Some(*tier);
            }
            sessions.remove(key);
        }
        None
    }

    pub fn update(&self, key: &str, tier: ComplexityTier) {
        let Ok(mut sessions) = self.sessions.lock() else {
            return;
        };
        if sessions.len() >= self.max_entries {
            if let Some(oldest_key) = sessions
                .iter()
                .min_by_key(|(_, (_, instant))| *instant)
                .map(|(k, _)| k.clone())
            {
                sessions.remove(&oldest_key);
            }
        }
        let current = sessions.get(key).map(|(t, _)| *t);
        let new_tier = current.map(|c| c.max(tier)).unwrap_or(tier);
        sessions.insert(key.to_string(), (new_tier, Instant::now()));
    }
}

pub fn session_key(request: &ChatCompletionRequest) -> Option<String> {
    if let Some(user) = &request.user {
        if !user.is_empty() {
            return Some(format!("user:{}", user));
        }
    }
    request.messages.iter().find(|m| m.role == "user").map(|m| {
        let content = match &m.content {
            MessageContent::Text(t) => t.clone(),
            MessageContent::Parts(p) => p
                .iter()
                .map(|part| match part {
                    miser_types::ContentPart::Text { text } => text.clone(),
                    _ => String::new(),
                })
                .collect::<Vec<_>>()
                .join(" "),
        };
        let hash = fnv_hash(content.as_bytes());
        format!("msg:{:x}", hash)
    })
}

fn fnv_hash(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 14695981039346656037;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn session_tracker_keeps_max_tier() {
        let tracker = SessionTracker::new(100, 60);
        tracker.update("s1", ComplexityTier::Hard);
        assert_eq!(tracker.get("s1"), Some(ComplexityTier::Hard));
        tracker.update("s1", ComplexityTier::Simple);
        assert_eq!(
            tracker.get("s1"),
            Some(ComplexityTier::Hard),
            "should not downgrade"
        );
        tracker.update("s1", ComplexityTier::Reasoning);
        assert_eq!(tracker.get("s1"), Some(ComplexityTier::Reasoning));
    }

    #[test]
    fn session_key_uses_user_field() {
        let request: ChatCompletionRequest = serde_json::from_value(json!({
            "model": "auto",
            "messages": [{"role":"user","content":"hello"}],
            "user": "session-abc"
        }))
        .unwrap();
        assert_eq!(session_key(&request), Some("user:session-abc".into()));
    }

    #[test]
    fn session_key_falls_back_to_first_message() {
        let request: ChatCompletionRequest = serde_json::from_value(json!({
            "model": "auto",
            "messages": [{"role":"user","content":"build the project"}]
        }))
        .unwrap();
        let key = session_key(&request);
        assert!(key.is_some());
        assert!(key.unwrap().starts_with("msg:"));
    }
}
