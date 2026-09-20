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
    // Jev's typed score question returns a probability-weighted level index
    // on a 0-4 scale; judges emitting 0-1 pass through unchanged. Without
    // the normalization every Jev score >= 1 clamps to 1.0 and never fails
    // the threshold.
    let raw = result.score;
    let score = (if raw > 1.0 { raw / 4.0 } else { raw }).clamp(0.0, 1.0);
    Some(QualityScore {
        score,
        passed: result.passed && score >= config.minimum_score,
        reason: "llm-judge",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(minimum: f32) -> QualityConfig {
        QualityConfig {
            enabled: true,
            minimum_score: minimum,
            ..Default::default()
        }
    }

    #[test]
    fn jev_five_level_scores_are_normalized() {
        // A level-2 response (0-4 scale) must normalize to 0.5, not clamp
        // to 1.0 and pass every threshold.
        let score = parse_judge(r#"{"score": 2.17, "passed": false}"#, &config(0.65)).unwrap();
        assert!((score.score - 2.17 / 4.0).abs() < 1e-6);
        assert!(!score.passed);
    }

    #[test]
    fn zero_one_scores_pass_through_unscaled() {
        let score = parse_judge(r#"{"score": 0.5, "passed": true}"#, &config(0.65)).unwrap();
        assert!((score.score - 0.5).abs() < 1e-6);
        assert!(!score.passed);
    }

    #[test]
    fn strong_jev_scores_pass() {
        let score = parse_judge(r#"{"score": 4.0, "passed": true}"#, &config(0.65)).unwrap();
        assert!((score.score - 1.0).abs() < 1e-6);
        assert!(score.passed);
    }
}
