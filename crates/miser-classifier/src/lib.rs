use miser_types::{
    ChatCompletionRequest, ClassificationResult, ClassifierConfig, ClassifierMode, ComplexityTier,
    MessageContent, TaskType,
};
use regex::RegexSet;
use serde::Deserialize;
use serde_json::json;
use std::time::Instant;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClassifierError {
    #[error("classifier request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("classifier returned invalid response: {0}")]
    Parse(#[from] serde_json::Error),
}

#[derive(Clone)]
pub struct Classifier {
    config: ClassifierConfig,
    client: reqwest::Client,
    trivial: RegexSet,
    simple: RegexSet,
    standard: RegexSet,
    hard: RegexSet,
    reasoning: RegexSet,
}

#[derive(Debug, Deserialize)]
struct LlmResult {
    tier: ComplexityTier,
    #[serde(default = "default_confidence")]
    confidence: f32,
    #[serde(default)]
    reason: String,
}

fn default_confidence() -> f32 {
    0.7
}

impl Classifier {
    pub fn new(config: ClassifierConfig) -> Result<Self, regex::Error> {
        Ok(Self {
            config,
            client: reqwest::Client::builder()
                .build()
                .expect("client construction"),
            trivial: RegexSet::new([
                r"(?i)^\s*(hello|hi|hey|thanks|thank you|ok|okay)\s*[!.]*\s*$",
                r"(?i)\b(git status|git diff|git log|list (the )?files?)\b",
                r"(?i)^(what is|what's)\s+[^?]{0,80}\??$",
                r"(?i)\b(rename|uppercase|lowercase|trim)\b.*\b(variable|file|string|line)\b",
                r"(?i)^\s*(yes|no|true|false)\s*[.!]?\s*$",
            ])?,
            simple: RegexSet::new([
                r"(?i)\b(explain|summarize|compare|convert|translate|format)\b",
                r"(?i)\b(write|create)\s+(a|an)\s+(small|simple)?\s*(function|class|regex|script|interface)\b",
                r"(?i)\b(add|change|fix)\s+(a|the)\s+(comment|null check|format)\b",
            ])?,
            standard: RegexSet::new([
                r"(?i)\b(implement|build|integrate|debug|refactor|test|endpoint|migration)\b",
                r"(?i)\b(api|database|authentication|middleware|component)\b.*\b(add|create|implement|design)\b",
            ])?,
            hard: RegexSet::new([
                r"(?i)\b(architect|distributed|production incident|threat-model|zero-downtime|multi-region)\b",
                r"(?i)\b(security|concurrency|race condition|migration|rollout|failover)\b.*\b(design|analy[sz]e|plan|fix)\b",
                r"(?i)\b(one million|40 services|80-file|across (all|every|five))\b",
            ])?,
            reasoning: RegexSet::new([
                r"(?i)\b(prove|derive|counterexample|formal|satisfiable|optimality|correctness)\b",
                r"(?i)\b(algorithm|recurrence|serialization graph|posterior|inference)\b.*\b(analysis|design|prove|derive|bound)\b",
            ])?,
        })
    }

    pub async fn classify(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<ClassificationResult, ClassifierError> {
        let started = Instant::now();
        let text = request_text(request);
        if let Some((tier, reason)) = override_tier(&text) {
            return Ok(result(
                tier,
                1.0,
                "override",
                vec![reason],
                started,
                task(&text),
            ));
        }

        let heuristic = self.heuristic(&text, request, started);
        match self.config.mode {
            ClassifierMode::Heuristic => Ok(heuristic),
            ClassifierMode::LocalLlm => self
                .llm(request, &self.config.local_llm, "local_llm", started)
                .await
                .or(Ok(heuristic)),
            ClassifierMode::CloudLlm => self
                .llm(request, &self.config.cloud_llm, "cloud_llm", started)
                .await
                .or(Ok(heuristic)),
            ClassifierMode::Hybrid => {
                if heuristic.confidence >= self.config.confidence_threshold {
                    return Ok(heuristic);
                }
                let local_fut = if self.config.local_llm.enabled {
                    Some(Box::pin(self.llm(
                        request,
                        &self.config.local_llm,
                        "local_llm",
                        started,
                    )))
                } else {
                    None
                };
                let cloud_fut = if self.config.cloud_llm.enabled {
                    Some(Box::pin(self.llm(
                        request,
                        &self.config.cloud_llm,
                        "cloud_llm",
                        started,
                    )))
                } else {
                    None
                };
                match (local_fut, cloud_fut) {
                    (Some(local), Some(cloud)) => {
                        let mut local = local;
                        let mut cloud = cloud;
                        tokio::select! {
                            result = &mut local => match result {
                                Ok(r) if r.confidence >= self.config.confidence_threshold => Ok(r),
                                Ok(local_result) => {
                                    match cloud.as_mut().await {
                                        Ok(cloud_result) if cloud_result.confidence >= self.config.confidence_threshold => Ok(cloud_result),
                                        Ok(_) => Ok(local_result),
                                        Err(_) => Ok(local_result),
                                    }
                                }
                                Err(_) => match cloud.as_mut().await {
                                    Ok(r) => Ok(r),
                                    Err(_) => Ok(heuristic),
                                },
                            },
                            result = &mut cloud => match result {
                                Ok(r) if r.confidence >= self.config.confidence_threshold => Ok(r),
                                Ok(cloud_result) => match local.as_mut().await {
                                    Ok(local_result) if local_result.confidence >= self.config.confidence_threshold => Ok(local_result),
                                    Ok(_) => Ok(cloud_result),
                                    Err(_) => Ok(cloud_result),
                                },
                                Err(_) => match local.as_mut().await {
                                    Ok(r) => Ok(r),
                                    Err(_) => Ok(heuristic),
                                },
                            },
                        }
                    }
                    (Some(mut local), None) => local.as_mut().await.or(Ok(heuristic)),
                    (None, Some(mut cloud)) => cloud.as_mut().await.or(Ok(heuristic)),
                    (None, None) => Ok(heuristic),
                }
            }
        }
    }

    fn heuristic(
        &self,
        text: &str,
        request: &ChatCompletionRequest,
        started: Instant,
    ) -> ClassificationResult {
        let mut reasons = Vec::new();
        let mut scores = [
            (ComplexityTier::Trivial, 0_i32),
            (ComplexityTier::Simple, 0),
            (ComplexityTier::Standard, 0),
            (ComplexityTier::Hard, 0),
            (ComplexityTier::Reasoning, 0),
        ];
        let sets = [
            (&self.trivial, 0, 5),
            (&self.simple, 1, 3),
            (&self.standard, 2, 4),
            (&self.hard, 3, 6),
            (&self.reasoning, 4, 7),
        ];
        for (set, index, weight) in sets {
            let matches = set.matches(text).into_iter().count() as i32;
            scores[index].1 += matches * weight;
            if matches > 0 {
                reasons.push(format!("pattern:{}:{}", index, matches));
            }
        }
        if request.tools.as_ref().is_some_and(|x| !x.is_empty()) {
            scores[2].1 += 4;
            reasons.push("tools-present".into());
        }
        if request.messages.len() > 10 {
            scores[2].1 += 3;
            reasons.push("deep-conversation".into());
        }
        if request.response_format.is_some() {
            scores[2].1 += 2;
            reasons.push("structured-output".into());
        }
        let last = scores
            .iter()
            .max_by_key(|(_, score)| *score)
            .copied()
            .unwrap_or((ComplexityTier::Standard, 0));
        let confidence = if last.1 == 0 {
            0.0
        } else {
            (0.55 + last.1 as f32 / 30.0).min(0.95)
        };
        result(
            last.0,
            confidence,
            "heuristic",
            reasons,
            started,
            task(text),
        )
    }

    async fn llm(
        &self,
        request: &ChatCompletionRequest,
        endpoint: &miser_types::ClassifierEndpointConfig,
        name: &str,
        started: Instant,
    ) -> Result<ClassificationResult, ClassifierError> {
        if !endpoint.enabled || endpoint.base_url.is_empty() || endpoint.model.is_empty() {
            return Err(ClassifierError::Parse(
                serde_json::from_str::<serde_json::Value>("null").unwrap_err(),
            ));
        }
        let body = json!({ "model": endpoint.model, "messages": [{"role":"system","content":"Classify minimum required capability. Return only JSON {tier: trivial|simple|standard|hard|reasoning, confidence: number, reason: string}. Judge work required, not keywords."},{"role":"user","content":request_text(request)}], "temperature":0, "max_tokens":180, "think":false, "response_format":{"type":"json_object"} });
        let mut req = self
            .client
            .post(format!(
                "{}/chat/completions",
                endpoint.base_url.trim_end_matches('/')
            ))
            .timeout(std::time::Duration::from_millis(endpoint.timeout_ms))
            .json(&body);
        if let Some(key) = &endpoint.api_key {
            if !key.is_empty() {
                req = req.bearer_auth(key);
            }
        }
        let payload: serde_json::Value = req.send().await?.error_for_status()?.json().await?;
        let content = payload["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default();
        let parsed: LlmResult = serde_json::from_str(content)
            .or_else(|_| serde_json::from_str(content.trim_matches('`').trim()))?;
        Ok(result(
            parsed.tier,
            parsed.confidence.clamp(0.0, 0.99),
            name,
            vec![parsed.reason],
            started,
            task(&request_text(request)),
        ))
    }
}

fn request_text(request: &ChatCompletionRequest) -> String {
    request
        .messages
        .iter()
        .map(|message| match &message.content {
            MessageContent::Text(text) => text.clone(),
            MessageContent::Parts(parts) => parts
                .iter()
                .map(|part| match part {
                    miser_types::ContentPart::Text { text } => text.clone(),
                    _ => String::new(),
                })
                .collect::<Vec<_>>()
                .join(" "),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn override_tier(text: &str) -> Option<(ComplexityTier, String)> {
    let first = text.lines().next()?.trim();
    let tier = first.strip_prefix("@route:")?;
    let parsed = match tier {
        "trivial" => ComplexityTier::Trivial,
        "simple" => ComplexityTier::Simple,
        "standard" => ComplexityTier::Standard,
        "hard" => ComplexityTier::Hard,
        "reasoning" => ComplexityTier::Reasoning,
        _ => return None,
    };
    Some((parsed, format!("override:{tier}")))
}

fn task(text: &str) -> Option<TaskType> {
    let lower = text.to_lowercase();
    if lower.contains("code") || lower.contains("implement") || lower.contains("function") {
        Some(TaskType::Coding)
    } else if lower.contains("prove") || lower.contains("derive") || lower.contains("algorithm") {
        Some(TaskType::Reasoning)
    } else {
        Some(TaskType::Chat)
    }
}

fn result(
    tier: ComplexityTier,
    confidence: f32,
    classifier: &str,
    reasons: Vec<String>,
    started: Instant,
    task: Option<TaskType>,
) -> ClassificationResult {
    ClassificationResult {
        tier,
        confidence,
        reasons,
        classifier: classifier.into(),
        latency_ms: started.elapsed().as_millis() as u64,
        task,
        risk: None,
        privacy: None,
        extra: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miser_types::ChatCompletionRequest;

    fn request(text: &str) -> ChatCompletionRequest {
        serde_json::from_value(json!({"model":"auto","messages":[{"role":"user","content":text}]}))
            .unwrap()
    }

    #[tokio::test]
    async fn classifies_representative_tiers() {
        let mut config = ClassifierConfig {
            mode: ClassifierMode::Heuristic,
            ..serde_json::from_str("{}").unwrap()
        };
        config.confidence_threshold = 0.7;
        let classifier = Classifier::new(config).unwrap();
        assert_eq!(
            classifier.classify(&request("Hello")).await.unwrap().tier,
            ComplexityTier::Trivial
        );
        assert_eq!(
            classifier
                .classify(&request("Explain DNS"))
                .await
                .unwrap()
                .tier,
            ComplexityTier::Simple
        );
        assert_eq!(
            classifier
                .classify(&request("Implement an API endpoint"))
                .await
                .unwrap()
                .tier,
            ComplexityTier::Standard
        );
        assert_eq!(
            classifier
                .classify(&request("Architect a distributed cache"))
                .await
                .unwrap()
                .tier,
            ComplexityTier::Hard
        );
        assert_eq!(
            classifier
                .classify(&request("Prove this algorithm is optimal"))
                .await
                .unwrap()
                .tier,
            ComplexityTier::Reasoning
        );
    }

    #[tokio::test]
    async fn valid_override_wins() {
        let config = ClassifierConfig {
            mode: ClassifierMode::Heuristic,
            ..serde_json::from_str("{}").unwrap()
        };
        let classifier = Classifier::new(config).unwrap();
        assert_eq!(
            classifier
                .classify(&request("@route:trivial\nProve a theorem"))
                .await
                .unwrap()
                .tier,
            ComplexityTier::Trivial
        );
    }
}
