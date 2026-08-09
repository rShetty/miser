use miser_types::{
    ChatCompletionRequest, ClassificationResult, ComplexityTier, GatewayConfig,
    TierModelRouteConfig,
};
use thiserror::Error;

pub mod quality;

#[derive(Debug, Error)]
pub enum PolicyError {
    #[error("no model route configured for tier {0:?}")]
    MissingRoute(ComplexityTier),
}

#[derive(Clone)]
pub struct PolicyEngine {
    config: GatewayConfig,
}

impl PolicyEngine {
    pub fn new(config: GatewayConfig) -> Self {
        Self { config }
    }

    pub fn select(
        &self,
        request: &ChatCompletionRequest,
        classification: &ClassificationResult,
    ) -> Result<TierModelRouteConfig, PolicyError> {
        let tier = self.effective_tier(request, classification);
        self.config
            .tiers
            .get(&tier)
            .cloned()
            .ok_or(PolicyError::MissingRoute(tier))
    }

    pub fn next(
        &self,
        request: &ChatCompletionRequest,
        classification: &ClassificationResult,
    ) -> Result<Option<TierModelRouteConfig>, PolicyError> {
        let tier = self.effective_tier(request, classification);
        let Some(next_tier) = next_tier(tier) else {
            return Ok(None);
        };
        self.config
            .tiers
            .get(&next_tier)
            .cloned()
            .map(Some)
            .ok_or(PolicyError::MissingRoute(next_tier))
    }

    pub fn effective_tier(
        &self,
        request: &ChatCompletionRequest,
        classification: &ClassificationResult,
    ) -> ComplexityTier {
        let mut tier = classification.tier;
        if classification.confidence < self.config.classifier.confidence_threshold {
            tier = max_tier(tier, ComplexityTier::Standard);
        }
        if request
            .tools
            .as_ref()
            .is_some_and(|tools| !tools.is_empty())
        {
            tier = max_tier(tier, ComplexityTier::Standard);
        }
        if request.response_format.is_some() {
            tier = max_tier(tier, ComplexityTier::Standard);
        }
        if classification.task == Some(miser_types::TaskType::Reasoning) {
            tier = max_tier(tier, ComplexityTier::Reasoning);
        }
        tier
    }

    pub fn config(&self) -> &GatewayConfig {
        &self.config
    }
}

fn max_tier(left: ComplexityTier, right: ComplexityTier) -> ComplexityTier {
    left.max(right)
}

fn next_tier(tier: ComplexityTier) -> Option<ComplexityTier> {
    match tier {
        ComplexityTier::Trivial => Some(ComplexityTier::Simple),
        ComplexityTier::Simple => Some(ComplexityTier::Standard),
        ComplexityTier::Standard => Some(ComplexityTier::Hard),
        ComplexityTier::Hard => Some(ComplexityTier::Reasoning),
        ComplexityTier::Reasoning => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tools_get_a_standard_floor() {
        let request: ChatCompletionRequest = serde_json::from_value(json!({
            "model": "auto",
            "messages": [{"role":"user","content":"say hi"}],
            "tools": [{"type":"function"}]
        }))
        .unwrap();
        let classification = ClassificationResult {
            tier: ComplexityTier::Trivial,
            confidence: 0.99,
            reasons: vec![],
            classifier: "test".into(),
            latency_ms: 0,
            task: None,
            risk: None,
            privacy: None,
            extra: Default::default(),
        };
        let config: GatewayConfig =
            toml::from_str(include_str!("../../../config/miser.toml")).unwrap();
        let policy = PolicyEngine::new(config);
        assert_eq!(
            policy.effective_tier(&request, &classification),
            ComplexityTier::Standard
        );
    }
}
