use miser_types::{ChatCompletionRequest, ClassificationResult, QualityConfig, TaskType};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QualityScore {
    pub score: f32,
    pub passed: bool,
    pub reason: &'static str,
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
    let message = &response["choices"][0]["message"];
    let content = message["content"].as_str().unwrap_or_default();
    // A turn that only calls tools has `content: null` by design, so the
    // empty-content check below used to hard-fail every tool-calling
    // response (score 0.0) and force a pointless escalation. Emitting a
    // well-formed tool call is a complete answer, so score it as one.
    let tool_calls = message["tool_calls"].as_array();
    if content.trim().is_empty() {
        if tool_calls.is_some_and(|calls| !calls.is_empty()) {
            return QualityScore {
                score: 0.85,
                passed: true,
                reason: "tool-calls",
            };
        }
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

            security_risk: None,
            jev_model: None,
            classifier_cost_usd: None,
            extra: Default::default(),
        }
    }

    fn response(content: &str) -> Value {
        json!({"choices":[{"message":{"content":content}}]})
    }

    #[test]
    fn tool_call_response_passes_instead_of_failing_as_empty() {
        // A tool-calling turn carries `content: null` by design. Treating it
        // as an empty answer scored every agentic turn 0.0, which forced an
        // escalation to a weaker tier on every single tool call.
        let tool_call_response = json!({
            "choices":[{"message":{
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "grep", "arguments": "{\"pattern\":\"TODO\"}"}
                }]
            }}]
        });
        let score = deterministic_quality(
            &chat_request(None),
            &tool_call_response,
            &classification(Some(TaskType::Agentic)),
            &config(0.65),
        );
        assert!(score.passed, "tool call must not fail the quality gate");
        assert_eq!(score.reason, "tool-calls");
        assert!(score.score >= 0.65);

        // A genuinely empty response must still fail.
        let empty = deterministic_quality(
            &chat_request(None),
            &response(""),
            &classification(None),
            &config(0.65),
        );
        assert!(!empty.passed);
        assert_eq!(empty.reason, "empty-content");

        // An empty `tool_calls` array is not a substantive answer either.
        let empty_calls = deterministic_quality(
            &chat_request(None),
            &json!({"choices":[{"message":{"content":null,"tool_calls":[]}}]}),
            &classification(None),
            &config(0.65),
        );
        assert!(!empty_calls.passed);
        assert_eq!(empty_calls.reason, "empty-content");
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
}
