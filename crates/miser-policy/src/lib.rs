use miser_types::{ClassificationResult, ComplexityTier, GatewayConfig, TierModelRouteConfig};
use thiserror::Error;

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
        classification: &ClassificationResult,
    ) -> Result<TierModelRouteConfig, PolicyError> {
        self.config
            .tiers
            .get(&classification.tier)
            .cloned()
            .ok_or(PolicyError::MissingRoute(classification.tier))
    }

    pub fn config(&self) -> &GatewayConfig {
        &self.config
    }
}
