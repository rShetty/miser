//! Jev quality judge over produced responses.
//!
//! One typed `score` question against the same Jev endpoint the classifier
//! uses, asked about the response instead of the prompt. Returns the
//! probability-weighted level index normalized to 0..=1 so the
//! `minimum_score` threshold compares like with like.

use miser_types::{ClassifierEndpointConfig, ContentPart, MessageContent};
use serde_json::json;

pub struct QualityJudge {
    http: reqwest::Client,
    endpoint: ClassifierEndpointConfig,
    api_key: String,
}

impl Clone for QualityJudge {
    fn clone(&self) -> Self {
        Self {
            http: self.http.clone(),
            endpoint: self.endpoint.clone(),
            api_key: self.api_key.clone(),
        }
    }
}

const CRITERIA: [&str; 5] = [
    "Completely wrong, irrelevant, or empty - fails to address the prompt at all",
    "Major errors, missing most required concepts, or significant tangents",
    "Partially correct but with notable gaps in required concepts or minor errors",
    "Mostly correct and complete with only minor issues",
    "Correct, complete, and directly relevant - covers all required concepts accurately",
];

const SCORE_INSTRUCTIONS: &str = "Score this AI response for correctness (factually accurate, no hallucinations), completeness (covers all required concepts and addresses the full prompt), and relevance (directly answers what was asked, no tangents). Weight correctness highest, then completeness, then relevance.";

impl QualityJudge {
    pub fn new(endpoint: &ClassifierEndpointConfig, api_key: String) -> Self {
        let timeout = endpoint.timeout_ms.clamp(100, 10_000);
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(timeout))
            .build()
            .unwrap_or_default();
        Self {
            http,
            endpoint: endpoint.clone(),
            api_key,
        }
    }

    fn url(&self) -> String {
        let path = match self.endpoint.path.as_deref() {
            Some("") | None => "/evaluate",
            Some(p) if p.starts_with("http://") || p.starts_with("https://") => {
                return p.to_string();
            }
            Some(p) if p.starts_with('/') => p,
            Some(p) => &format!("/{p}"),
        };
        format!("{}{}", self.endpoint.base_url.trim_end_matches('/'), path)
    }

    /// Score one response. `None` on transport or parse failure — callers
    /// treat it as "judge unavailable" and fall back to deterministic checks.
    pub async fn score(&self, prompt: &str, response_text: &str) -> Option<f32> {
        let truncated: String = response_text.chars().take(3000).collect();
        let body = json!({
            "model": self.endpoint.model,
            "state": {
                "request": format!("Task: {prompt}\n\nResponse (truncated to 3000 chars): {truncated}"),
                "tools": [],
                "tool_history": false
            },
            "questions": {
                "quality": {
                    "type": "score",
                    "instructions": SCORE_INSTRUCTIONS,
                    "criteria": CRITERIA,
                }
            }
        });
        let url = self.url();
        let payload: serde_json::Value = self
            .http
            .post(&url)
            .json(&body)
            .bearer_auth(&self.api_key)
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json()
            .await
            .ok()?;
        let answer = &payload["answers"]["quality"];
        // TypeSafe direct: explicit score. Gateway contract: per-level
        // probabilities; weighted mean over the 0-4 level indices.
        let raw = answer["score"].as_f64().or_else(|| {
            let probs = answer["probabilities"].as_object()?;
            let total: f64 = probs.values().filter_map(|v| v.as_f64()).sum();
            if total <= 0.0 {
                return None;
            }
            Some(
                probs
                    .iter()
                    .map(|(level, prob)| {
                        level.parse::<f64>().unwrap_or(0.0) * prob.as_f64().unwrap_or(0.0)
                    })
                    .sum::<f64>()
                    / total,
            )
        })?;
        Some((if raw > 1.0 { raw / 4.0 } else { raw }).clamp(0.0, 1.0) as f32)
    }

    /// Jev equivalence validation for the semantic cache: asks whether the
    /// same response would satisfy both requests. `None` on transport or
    /// parse failure — callers treat it as "no hit".
    pub async fn equivalent(&self, prompt: &str, cached_prompt: &str) -> Option<bool> {
        let body = json!({
            "model": self.endpoint.model,
            "state": {
                "request": format!(
                    "Cached request: {}\n\nNew request: {}\n\nWould the exact same response fully and correctly satisfy the new request, or does the new request ask for something materially different?",
                    cached_prompt.chars().take(3000).collect::<String>(),
                    prompt.chars().take(3000).collect::<String>()
                ),
                "tools": [],
                "tool_history": false
            },
            "questions": {
                "equivalence": {
                    "type": "choice",
                    "instructions": "Decide whether both requests ask for the same thing such that one shared response answers both. Requests that differ only in trivial wording, whitespace, or phrasing of the identical task are equivalent. Requests that change the subject, add constraints, request a different artifact, or ask about a different entity are different.",
                    "criteria": {
                        "equivalent": "Both requests ask for the same task or question; one response correctly answers both",
                        "different": "The requests differ in subject, requested artifact, constraints, entities, or any material aspect that changes the correct response"
                    }
                }
            }
        });
        let url = self.url();
        let payload: serde_json::Value = self
            .http
            .post(&url)
            .json(&body)
            .bearer_auth(&self.api_key)
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json()
            .await
            .ok()?;
        Some(payload["answers"]["equivalence"]["choice"].as_str()? == "equivalent")
    }
}

/// Last user message text, for the judge's task description.
pub fn last_user_text(request: &miser_types::ChatCompletionRequest) -> String {
    request
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| match &m.content {
            MessageContent::Text(text) => text.clone(),
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|part| match part {
                    ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use miser_types::ChatCompletionRequest;

    #[test]
    fn last_user_text_prefers_the_final_user_message() {
        let request: ChatCompletionRequest = serde_json::from_value(json!({
            "model": "auto",
            "messages": [
                {"role": "user", "content": "first"},
                {"role": "assistant", "content": "reply"},
                {"role": "user", "content": "second"}
            ]
        }))
        .unwrap();
        assert_eq!(last_user_text(&request), "second");
    }
}

#[cfg(test)]
mod equivalence_tests {
    use super::*;
    use miser_types::ClassifierEndpointConfig;

    fn judge_for(base_url: &str) -> QualityJudge {
        let endpoint = ClassifierEndpointConfig {
            enabled: true,
            model: "jev-latest".into(),
            base_url: base_url.to_string(),
            api_key: Some("test-key".into()),
            path: Some("/evaluate".into()),
            timeout_ms: 2000,
            extra: Default::default(),
        };
        QualityJudge::new(&endpoint, "test-key".into())
    }

    /// One-connection mock Jev server answering with the given equivalence
    /// choice; returns the base URL.
    async fn spawn_mock(choice: &'static str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 8192];
            let mut data = Vec::new();
            loop {
                let n = sock.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                data.extend_from_slice(&buf[..n]);
                if data.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let payload = format!(
                r#"{{"answers": {{"equivalence": {{"choice": "{}"}}}}}}"#,
                choice
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
            let _ = sock.write_all(response.as_bytes()).await;
        });
        format!("http://{}", addr)
    }

    #[tokio::test]
    async fn equivalent_choice_maps_to_true() {
        let base = spawn_mock("equivalent").await;
        assert_eq!(judge_for(&base).equivalent("a", "b").await, Some(true));
    }

    #[tokio::test]
    async fn different_choice_maps_to_false() {
        let base = spawn_mock("different").await;
        assert_eq!(judge_for(&base).equivalent("a", "b").await, Some(false));
    }

    #[tokio::test]
    async fn transport_failure_is_none_not_error() {
        // Port 1 refuses connections; the judge must degrade to None so the
        // caller treats it as "no semantic hit" rather than an error.
        assert_eq!(
            judge_for("http://127.0.0.1:1").equivalent("a", "b").await,
            None
        );
    }
}
