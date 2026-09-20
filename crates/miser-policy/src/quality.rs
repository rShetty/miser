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
    use serde_json::json;

    fn config(minimum: f32) -> QualityConfig {
        QualityConfig {
            enabled: true,
            minimum_score: minimum,
            ..Default::default()
        }
    }

    fn chat_request(response_format: Option<serde_json::Value>) -> ChatCompletionRequest {
        let mut value = json!({
            "model": "m",
            "messages": [{"role":"user","content":"hi"}]
        });
        if let Some(format) = response_format {
            value["response_format"] = format;
        }
        serde_json::from_value(value).unwrap()
    }

    fn classification(task: Option<TaskType>) -> ClassificationResult {
        ClassificationResult {
            tier: miser_types::ComplexityTier::Trivial,
            confidence: 0.99,
            reasons: vec![],
            classifier: "test".into(),
            latency_ms: 0,
            task,
            risk: None,
            privacy: None,
            extra: Default::default(),
        }
    }

    fn response(content: &str) -> Value {
        json!({"choices":[{"message":{"content":content}}]})
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

    #[test]
    fn disabled_quality_passes_without_inspecting_the_response() {
        let config = QualityConfig {
            enabled: false,
            ..Default::default()
        };
        let score = deterministic_quality(
            &chat_request(None),
            &serde_json::Value::Null,
            &classification(None),
            &config,
        );
        assert!(score.passed);
        assert!((score.score - 1.0).abs() < 1e-6);
        assert_eq!(score.reason, "quality-disabled");
    }

    #[test]
    fn empty_and_missing_content_fail() {
        let config = config(0.7);
        let cls = classification(None);
        for response in [json!({}), response("   \n\t")] {
            let score = deterministic_quality(&chat_request(None), &response, &cls, &config);
            assert!(!score.passed);
            assert_eq!(score.reason, "empty-content");
            assert_eq!(score.score, 0.0);
        }
    }

    #[test]
    fn structured_output_must_parse_as_json() {
        let format = json!({"type": "json_object"});
        let cls = classification(None);
        let config = config(0.7);
        let bad = deterministic_quality(
            &chat_request(Some(format.clone())),
            &response("ran out of tokens"),
            &cls,
            &config,
        );
        assert!(!bad.passed);
        assert_eq!(bad.reason, "invalid-json-output");
        assert_eq!(bad.score, 0.2);

        let good_content =
            r#"{"summary": "migration completed and verified across every service"}"#;
        let good = deterministic_quality(
            &chat_request(Some(format)),
            &response(good_content),
            &cls,
            &config,
        );
        assert!(good.passed);
        assert_eq!(good.reason, "deterministic-content-check");
    }

    #[test]
    fn coding_output_needs_code_markers_or_substance() {
        let cls = classification(Some(TaskType::Coding));
        let config = config(0.65);
        let thin = deterministic_quality(&chat_request(None), &response("done."), &cls, &config);
        assert!(!thin.passed);
        assert_eq!(thin.reason, "insufficient-output");
        assert_eq!(thin.score, 0.3);

        // A fenced snippet counts as code even when it is short.
        let coded = deterministic_quality(
            &chat_request(None),
            &response("```rust\nfn fix() {}\n```"),
            &cls,
            &config,
        );
        assert_eq!(coded.reason, "deterministic-content-check");
        assert_eq!(coded.score, 0.65);
        assert!(coded.passed);

        // Substantial prose also clears the code check.
        let prose = deterministic_quality(
            &chat_request(None),
            &response(&"a".repeat(80)),
            &cls,
            &config,
        );
        assert!(prose.passed);
        assert_eq!(prose.score, 0.85);
    }

    #[test]
    fn plain_responses_score_by_length_with_a_boundary_at_40_chars() {
        let cls = classification(None);
        let config = config(0.7);
        let short = deterministic_quality(
            &chat_request(None),
            &response(&"a".repeat(39)),
            &cls,
            &config,
        );
        assert_eq!(short.score, 0.65);
        assert!(!short.passed);
        let long = deterministic_quality(
            &chat_request(None),
            &response(&"a".repeat(40)),
            &cls,
            &config,
        );
        assert_eq!(long.score, 0.85);
        assert!(long.passed);
    }

    #[test]
    fn parse_judge_rejects_non_json_and_missing_fields() {
        let config = config(0.7);
        assert!(parse_judge("no verdict here", &config).is_none());
        assert!(parse_judge("{truncated", &config).is_none());
        assert!(parse_judge(r#"{"passed": true}"#, &config).is_none());
    }

    #[test]
    fn parse_judge_finds_json_embedded_in_prose() {
        let score =
            parse_judge("verdict: {\"score\": 3.0, \"passed\": true}", &config(0.7)).unwrap();
        assert!((score.score - 0.75).abs() < 1e-6);
        assert!(score.passed);
    }

    #[test]
    fn judge_pass_requires_both_the_flag_and_the_threshold() {
        let config = config(0.7);
        // A judge "passed" flag alone is not enough below the threshold.
        let low = parse_judge(r#"{"score": 0.5, "passed": true}"#, &config).unwrap();
        assert!(!low.passed);
        // The flag is mandatory even at a perfect score.
        let unflagged = parse_judge(r#"{"score": 1.0, "passed": false}"#, &config).unwrap();
        assert!(!unflagged.passed);
        // The threshold itself is inclusive.
        let exact = parse_judge(r#"{"score": 0.7, "passed": true}"#, &config).unwrap();
        assert!(exact.passed);
    }
}
