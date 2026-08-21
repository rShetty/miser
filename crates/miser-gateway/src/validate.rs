use miser_types::{ClassifierEndpointConfig, GatewayConfig};

/// Classifier stages the gateway knows how to execute. Anything else is a
/// config typo that would silently disable part of the pipeline.
pub const KNOWN_CLASSIFIER_STAGES: &[&str] = &[
    "override",
    "structural",
    "heuristic",
    "local_llm",
    "cloud_llm",
];

/// Fail-fast validation performed before the gateway binds a socket.
///
/// Collects every problem found so operators can fix the config in one pass
/// instead of discovering issues one startup attempt at a time.
pub fn validate_config(config: &GatewayConfig) -> Result<(), String> {
    let mut errors: Vec<String> = Vec::new();

    if config.tiers.is_empty() {
        errors.push("no tiers configured".to_string());
    }
    for (tier, route) in &config.tiers {
        if route.model.trim().is_empty() {
            errors.push(format!("tiers.{tier:?}.model must not be empty"));
        }
    }

    let mut seen_stages = std::collections::BTreeSet::new();
    for stage in &config.classifier.stages {
        if !KNOWN_CLASSIFIER_STAGES.contains(&stage.as_str()) {
            errors.push(format!(
                "classifier.stages contains unknown stage '{stage}' (known stages: {})",
                KNOWN_CLASSIFIER_STAGES.join(", ")
            ));
        }
        if !seen_stages.insert(stage.as_str()) {
            errors.push(format!(
                "classifier.stages contains duplicate stage '{stage}'"
            ));
        }
    }

    validate_endpoint(
        "classifier.local_llm",
        &config.classifier.local_llm,
        &mut errors,
    );
    validate_endpoint(
        "classifier.cloud_llm",
        &config.classifier.cloud_llm,
        &mut errors,
    );
    if let Some(judge) = &config.quality.judge {
        validate_endpoint("quality.judge", judge, &mut errors);
    }

    validate_base_url(
        "provider.base_url",
        &config.provider.base_url,
        true,
        &mut errors,
    );

    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "invalid configuration:\n  - {}",
            errors.join("\n  - ")
        ))
    }
}

fn validate_endpoint(prefix: &str, endpoint: &ClassifierEndpointConfig, errors: &mut Vec<String>) {
    // Derived `Default` leaves disabled endpoints with timeout_ms = 0, which
    // is harmless while no traffic can reach them; only fail-fast on
    // endpoints that are actually enabled.
    if endpoint.enabled {
        if endpoint.timeout_ms == 0 {
            errors.push(format!("{prefix}.timeout_ms must be greater than 0"));
        }
        if endpoint.model.trim().is_empty() {
            errors.push(format!("{prefix}.model must not be empty when enabled"));
        }
        validate_base_url(
            &format!("{prefix}.base_url"),
            &endpoint.base_url,
            true,
            errors,
        );
    } else if !endpoint.base_url.trim().is_empty() {
        // Still surface malformed URLs for disabled endpoints, but do not
        // demand one that was never configured.
        validate_base_url(
            &format!("{prefix}.base_url"),
            &endpoint.base_url,
            false,
            errors,
        );
    }
}

fn validate_base_url(key: &str, value: &str, required: bool, errors: &mut Vec<String>) {
    if value.trim().is_empty() {
        if required {
            errors.push(format!("{key} must not be empty"));
        }
        return;
    }
    match reqwest::Url::parse(value) {
        Ok(url) => match url.scheme() {
            "http" | "https" => {}
            scheme => errors.push(format!("{key} must use http or https (found '{scheme}')")),
        },
        Err(error) => errors.push(format!("{key} is not a valid URL ({error})")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miser_types::ComplexityTier;
    use serde_json::json;

    fn base_config() -> GatewayConfig {
        serde_json::from_value(json!({
            "host": "127.0.0.1",
            "port": 0,
            "classifier": {"mode": "heuristic"},
            "provider": {"api_key": "test-key"},
            "tiers": {
                "trivial": {"model": "test/trivial"},
                "simple": {"model": "test/simple"},
                "standard": {"model": "test/standard"},
                "hard": {"model": "test/hard"},
                "reasoning": {"model": "test/reasoning"}
            }
        }))
        .expect("base config parses")
    }

    #[test]
    fn accepts_valid_config() {
        assert_eq!(validate_config(&base_config()), Ok(()));
    }

    #[test]
    fn accepts_shipped_stage_list() {
        let mut config = base_config();
        config.classifier.stages = KNOWN_CLASSIFIER_STAGES
            .iter()
            .map(|stage| (*stage).to_string())
            .collect();
        assert_eq!(validate_config(&config), Ok(()));
    }

    #[test]
    fn rejects_empty_tier_model() {
        let mut config = base_config();
        config.tiers.get_mut(&ComplexityTier::Hard).unwrap().model = "   ".to_string();
        let error = validate_config(&config).unwrap_err();
        assert!(
            error.contains("tiers.Hard.model must not be empty"),
            "{error}"
        );
    }

    #[test]
    fn rejects_unknown_classifier_stage() {
        let mut config = base_config();
        config.classifier.stages = vec!["override".to_string(), "vibes".to_string()];
        let error = validate_config(&config).unwrap_err();
        assert!(error.contains("unknown stage 'vibes'"), "{error}");
        assert!(error.contains("known stages:"), "{error}");
    }

    #[test]
    fn rejects_duplicate_classifier_stage() {
        let mut config = base_config();
        config.classifier.stages = vec!["heuristic".to_string(), "heuristic".to_string()];
        let error = validate_config(&config).unwrap_err();
        assert!(error.contains("duplicate stage 'heuristic'"), "{error}");
    }

    #[test]
    fn rejects_zero_timeout_on_enabled_endpoint() {
        let mut config = base_config();
        config.classifier.local_llm.enabled = true;
        config.classifier.local_llm.model = "qwen3:1.7b".to_string();
        config.classifier.local_llm.base_url = "http://127.0.0.1:11434/v1".to_string();
        config.classifier.local_llm.timeout_ms = 0;
        let error = validate_config(&config).unwrap_err();
        assert!(
            error.contains("classifier.local_llm.timeout_ms must be greater than 0"),
            "{error}"
        );
    }

    #[test]
    fn allows_derived_default_timeout_on_disabled_endpoint() {
        // `ClassifierEndpointConfig::default()` yields timeout_ms = 0; a
        // disabled endpoint never issues requests so this must not block
        // startup.
        let mut config = base_config();
        config.classifier.local_llm.timeout_ms = 0;
        assert_eq!(validate_config(&config), Ok(()));
    }

    #[test]
    fn rejects_invalid_provider_base_url() {
        let mut config = base_config();
        config.provider.base_url = "not a url".to_string();
        let error = validate_config(&config).unwrap_err();
        assert!(
            error.contains("provider.base_url is not a valid URL"),
            "{error}"
        );
    }

    #[test]
    fn rejects_non_http_scheme() {
        let mut config = base_config();
        config.provider.base_url = "ftp://openrouter.ai/api/v1".to_string();
        let error = validate_config(&config).unwrap_err();
        assert!(
            error.contains("provider.base_url must use http or https"),
            "{error}"
        );
    }

    #[test]
    fn enabled_endpoint_requires_model_and_base_url() {
        let mut config = base_config();
        config.classifier.cloud_llm.enabled = true;
        config.classifier.cloud_llm.timeout_ms = 1500;
        let error = validate_config(&config).unwrap_err();
        assert!(
            error.contains("classifier.cloud_llm.model must not be empty"),
            "{error}"
        );
        assert!(
            error.contains("classifier.cloud_llm.base_url must not be empty"),
            "{error}"
        );
    }

    #[test]
    fn reports_all_errors_at_once() {
        let mut config = base_config();
        config
            .tiers
            .get_mut(&ComplexityTier::Trivial)
            .unwrap()
            .model = String::new();
        config.classifier.stages = vec!["nope".to_string()];
        config.classifier.local_llm.enabled = true;
        config.classifier.local_llm.model = "qwen3:1.7b".to_string();
        config.classifier.local_llm.base_url = "http://127.0.0.1:11434/v1".to_string();
        config.classifier.local_llm.timeout_ms = 0;
        config.provider.base_url = ":::".to_string();
        let error = validate_config(&config).unwrap_err();
        assert!(error.contains("tiers.Trivial.model"), "{error}");
        assert!(error.contains("unknown stage 'nope'"), "{error}");
        assert!(error.contains("classifier.local_llm.timeout_ms"), "{error}");
        assert!(error.contains("provider.base_url"), "{error}");
        assert_eq!(error.matches("  - ").count(), 4, "{error}");
    }
}
