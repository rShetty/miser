use miser_types::{ChatCompletionRequest, ClassificationResult, QualityConfig, TaskType};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QualityScore {
    pub score: f32,
    pub passed: bool,
    pub reason: &'static str,
}

#[derive(Debug, Deserialize)]
struct JudgeResult {
    score: f32,
    passed: bool,
}

pub fn deterministic_quality(
    request: &ChatCompletionRequest,
    response: &Value,
    classification: &ClassificationResult,
    config: &QualityConfig,
) -> QualityScore {
    if !config.enabled {
        return QualityScore {
            score: 1.0,
            passed: true,
            reason: "quality-disabled",
        };
    }
    let content = response["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default();
    if content.trim().is_empty() {
        return QualityScore {
            score: 0.0,
            passed: false,
            reason: "empty-content",
        };
    }
    if request.response_format.is_some() {
        let parsed = serde_json::from_str::<Value>(content);
        if parsed.is_err() {
            return QualityScore {
                score: 0.2,
                passed: false,
                reason: "invalid-json-output",
            };
        }
    }
    if classification.task == Some(TaskType::Coding)
        || classification.task == Some(TaskType::Agentic)
        || content.contains("```")
    {
        let has_code = content.contains("```")
            || content.contains("fn ")
            || content.contains("function ")
            || content.contains("def ")
            || content.contains("tool_call")
            || content.contains("```shell")
            || content.contains("```bash");
        if !has_code && content.len() < 80 {
            return QualityScore {
                score: 0.3,
                passed: false,
                reason: "insufficient-output",
            };
        }
    }
    let score = if content.len() >= 40 { 0.85 } else { 0.65 };
    QualityScore {
        score,
        passed: score >= config.minimum_score,
        reason: "deterministic-content-check",
    }
}

pub fn parse_judge(content: &str, config: &QualityConfig) -> Option<QualityScore> {
    let json = content
        .match_indices('{')
        .next()
        .map(|(start, _)| &content[start..])?;
    let result = serde_json::from_str::<JudgeResult>(json).ok()?;
    let score = result.score.clamp(0.0, 1.0);
    Some(QualityScore {
        score,
        passed: result.passed && score >= config.minimum_score,
        reason: "llm-judge",
    })
}
