use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub type ExtraFields = BTreeMap<String, Value>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ImageUrl },
    #[serde(rename = "input_audio")]
    InputAudio { input_audio: Value },
    #[serde(rename = "refusal")]
    Refusal { refusal: String },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageUrl {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatMessage {
    pub role: String,
    pub content: MessageContent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ModelCapabilities {
    #[serde(default)]
    pub chat: bool,
    #[serde(default)]
    pub tools: bool,
    #[serde(default)]
    pub vision: bool,
    #[serde(default)]
    pub json_mode: bool,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "lowercase")]
pub enum ComplexityTier {
    Trivial,
    Simple,
    Standard,
    Hard,
    Reasoning,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    Chat,
    Coding,
    Agentic,
    Analysis,
    Creative,
    Summarization,
    Translation,
    Extraction,
    Planning,
    Reasoning,
    Classification,
    Other,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyLevel {
    Public,
    Internal,
    Confidential,
    Restricted,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum LatencyClass {
    Interactive,
    Standard,
    Background,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClassificationResult {
    pub tier: ComplexityTier,
    pub confidence: f32,
    pub reasons: Vec<String>,
    pub classifier: String,
    pub latency_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<RiskLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privacy: Option<PrivacyLevel>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ClassifierMode {
    Heuristic,
    LocalLlm,
    CloudLlm,
    Jev,
    Hybrid,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ClassifierEndpointConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// URL path appended to base_url. Defaults to `/evaluate` (Vercel AI
    /// Gateway); TypeSafe direct uses `/systemone`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

fn default_timeout_ms() -> u64 {
    30_000
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClassifierConfig {
    #[serde(default = "default_classifier_mode")]
    pub mode: ClassifierMode,
    #[serde(default)]
    pub stages: Vec<String>,
    #[serde(default = "default_confidence_threshold")]
    pub confidence_threshold: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ml_model: Option<String>,
    #[serde(default)]
    pub local_llm: ClassifierEndpointConfig,
    #[serde(default)]
    pub cloud_llm: ClassifierEndpointConfig,
    /// Jev (TypeSafe System One) evaluation endpoint. Talks the evaluation
    /// contract (`POST {base_url}/evaluate`), not chat completions.
    #[serde(default)]
    pub jev: ClassifierEndpointConfig,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

fn default_classifier_mode() -> ClassifierMode {
    ClassifierMode::Jev
}
fn default_confidence_threshold() -> f32 {
    0.55
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TierModelRouteConfig {
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_per_1m: Option<CostLimit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CostLimit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<f64>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ProviderConfig {
    pub api_key: String,
    #[serde(default = "default_openrouter_url")]
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_preferences: Option<ProviderPreferences>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

fn default_openrouter_url() -> String {
    "https://openrouter.ai/api/v1".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ProviderPreferences {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_fallbacks: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignore: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_price: Option<CostLimit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantizations: Option<Vec<String>>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GatewayConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    pub classifier: ClassifierConfig,
    pub tiers: BTreeMap<ComplexityTier, TierModelRouteConfig>,
    pub provider: ProviderConfig,
    /// Catalog routing configuration. Absent from a TOML file means
    /// `mode = "fixed"` (the pre-catalog tier→model mapping).
    #[serde(default)]
    pub routing: RoutingConfig,
    #[serde(default)]
    pub quality: QualityConfig,
    #[serde(default)]
    pub cache: CacheConfig,
    #[serde(default)]
    pub session: SessionConfig,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RoutingMode {
    #[default]
    Fixed,
    Catalog,
}

/// Catalog routing: every OpenRouter model is banded into the five
/// complexity tiers by input price; each tier pins one model and sticks to
/// it (no per-request switching) until an explicit refresh applies
/// hysteresis-gated migration or repeated upstream failures trigger
/// failover to the next candidate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoutingConfig {
    #[serde(default)]
    pub mode: RoutingMode,
    /// Where the persisted catalog snapshot (pins + full tier split) is
    /// stored. Defaults to `catalog/models.json` relative to the working
    /// directory, overridable via `MISER_CATALOG_FILE`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_path: Option<String>,
    /// Fetch a fresh catalog from the provider at startup. Failure keeps
    /// the persisted snapshot (or seeded pins) and is logged, never fatal.
    #[serde(default)]
    pub refresh_on_start: bool,
    /// First snapshot seeds tier pins from the fixed `[tiers.*].model`
    /// entries instead of picking the cheapest candidate, so enabling
    /// catalog mode changes nothing until a refresh deliberately migrates.
    #[serde(default = "default_true")]
    pub seed_from_config: bool,
    /// A refresh migrates a tier pin to a cheaper candidate only when the
    /// candidate's input price is at least this fraction cheaper than the
    /// current pin's (hysteresis against price noise).
    #[serde(default = "default_switch_saving_ratio")]
    pub switch_saving_ratio: f32,
    /// Consecutive upstream failures (5xx/429/transport) on a tier's active
    /// model before failover promotes the next candidate for that tier.
    #[serde(default = "default_failover_threshold")]
    pub failover_threshold: u32,
    #[serde(default)]
    pub bands: RoutingBands,
    #[serde(default)]
    pub filters: RoutingFilters,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

fn default_switch_saving_ratio() -> f32 {
    0.25
}
fn default_failover_threshold() -> u32 {
    3
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            mode: RoutingMode::Fixed,
            cache_path: None,
            refresh_on_start: false,
            seed_from_config: true,
            switch_saving_ratio: default_switch_saving_ratio(),
            failover_threshold: default_failover_threshold(),
            bands: RoutingBands::default(),
            filters: RoutingFilters::default(),
            extra: ExtraFields::default(),
        }
    }
}

/// Input-price bands (USD per 1M prompt tokens) splitting the catalog into
/// the five complexity tiers. Defaults are geometric midpoints between the
/// shipped tier anchor models (qwen3.7-flash $0.03, deepseek-v4-flash $0.04,
/// qwen3-coder-flash $0.20, glm-5.2 $0.65, claude-sonnet-4 $3.00).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoutingBands {
    #[serde(default = "default_trivial_max")]
    pub trivial_max: f64,
    #[serde(default = "default_simple_max")]
    pub simple_max: f64,
    #[serde(default = "default_standard_max")]
    pub standard_max: f64,
    #[serde(default = "default_reasoning_max")]
    pub reasoning_max: f64,
}

fn default_trivial_max() -> f64 {
    0.035
}
fn default_simple_max() -> f64 {
    0.09
}
fn default_standard_max() -> f64 {
    0.36
}
fn default_reasoning_max() -> f64 {
    1.4
}

impl Default for RoutingBands {
    fn default() -> Self {
        Self {
            trivial_max: default_trivial_max(),
            simple_max: default_simple_max(),
            standard_max: default_standard_max(),
            reasoning_max: default_reasoning_max(),
        }
    }
}

/// Hard filters a catalog model must pass to become a tier candidate (pin
/// or failover target). Non-passing models still appear in the tier split
/// for observability but are never routed to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoutingFilters {
    /// Require tool-calling support (`supported_parameters` includes
    /// "tools") — mandatory for coding-agent traffic.
    #[serde(default = "default_true")]
    pub require_tools: bool,
    /// Minimum advertised context window in tokens.
    #[serde(default = "default_min_context")]
    pub min_context: u64,
    /// Allow zero-priced and `:free`-suffixed models as candidates. Off by
    /// default: free endpoints are heavily rate-limited.
    #[serde(default)]
    pub allow_free: bool,
    /// Skip models whose id starts with any of these prefixes.
    #[serde(default)]
    pub deny_prefixes: Vec<String>,
    /// For the reasoning tier's pin, prefer models advertising reasoning
    /// support when any exist in the band.
    #[serde(default = "default_true")]
    pub prefer_reasoning_pin: bool,
}

fn default_min_context() -> u64 {
    65_536
}

impl Default for RoutingFilters {
    fn default() -> Self {
        Self {
            require_tools: true,
            min_context: default_min_context(),
            allow_free: false,
            deny_prefixes: Vec::new(),
            prefer_reasoning_pin: true,
        }
    }
}

/// One model entry extracted from the provider catalog, normalized for
/// banding and filtering.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CatalogModel {
    pub id: String,
    pub name: String,
    pub context_length: u64,
    /// USD per 1M prompt tokens.
    pub input_price_per_m: f64,
    /// USD per 1M completion tokens.
    pub output_price_per_m: f64,
    pub tools: bool,
    pub reasoning: bool,
    pub text_output: bool,
}

/// Sticky per-tier routing state: the pinned model plus an ordered
/// (cheapest-first) candidate list used for failover promotion.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TierPin {
    pub model: String,
    pub candidates: Vec<String>,
}

/// Persisted catalog snapshot: the full model→tier split plus the sticky
/// pin per tier. Written by catalog refresh, loaded at startup.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CatalogSnapshot {
    /// Unix seconds of the provider fetch; `None` for a seeded snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<u64>,
    /// "openrouter" after a live fetch, "seed" for config-derived pins.
    pub source: String,
    pub pins: BTreeMap<ComplexityTier, TierPin>,
    /// Every catalog model mapped to its price band tier.
    pub model_tiers: BTreeMap<String, ComplexityTier>,
    pub total_models: usize,
    /// Models that failed the candidate filters (still present in
    /// `model_tiers`, never routed to).
    pub unranked_models: usize,
}

fn default_host() -> String {
    "127.0.0.1".into()
}
fn default_port() -> u16 {
    8787
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QualityConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_quality_threshold")]
    pub minimum_score: f32,
    #[serde(default)]
    pub escalate_on_failure: bool,
    #[serde(default)]
    pub judge: Option<ClassifierEndpointConfig>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

impl Default for QualityConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            minimum_score: 0.7,
            escalate_on_failure: true,
            judge: None,
            extra: ExtraFields::new(),
        }
    }
}

fn default_quality_threshold() -> f32 {
    0.7
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CacheConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_cache_entries")]
    pub max_entries: usize,
    #[serde(default = "default_similarity")]
    pub similarity_threshold: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_model: Option<String>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_entries: 10_000,
            similarity_threshold: 0.92,
            embedding_model: None,
            extra: ExtraFields::new(),
        }
    }
}
fn default_true() -> bool {
    true
}
fn default_cache_entries() -> usize {
    10_000
}
fn default_similarity() -> f32 {
    0.92
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_session_ttl_seconds")]
    pub ttl_seconds: u64,
    #[serde(default = "default_session_max_entries")]
    pub max_entries: usize,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            ttl_seconds: 1800,
            max_entries: 10_000,
        }
    }
}

fn default_session_ttl_seconds() -> u64 {
    1800
}
fn default_session_max_entries() -> usize {
    10_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_preserves_unknown_fields() {
        let request: ChatCompletionRequest = serde_json::from_str(r#"{"model":"auto","messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}],"parallel_tool_calls":true}"#).unwrap();
        assert_eq!(request.extra["parallel_tool_calls"], true);
        assert!(matches!(
            request.messages[0].content,
            MessageContent::Parts(_)
        ));
    }

    #[test]
    fn tiers_are_ordered() {
        assert!(ComplexityTier::Trivial < ComplexityTier::Reasoning);
    }

    #[test]
    fn config_reads_toml_defaults() {
        let config: ProviderConfig = toml::from_str("api_key = 'secret'").unwrap();
        assert_eq!(config.base_url, "https://openrouter.ai/api/v1");
        let endpoint: ClassifierEndpointConfig = toml::from_str("").unwrap();
        assert_eq!(endpoint.timeout_ms, 30_000);
    }

    #[test]
    fn classifier_defaults_to_jev_mode() {
        let config: ClassifierConfig = toml::from_str("").unwrap();
        assert_eq!(config.mode, ClassifierMode::Jev);
        // Unknown fields still land in extra instead of failing the parse.
        let config: ClassifierConfig = toml::from_str("future_field = true").unwrap();
        assert_eq!(config.mode, ClassifierMode::Jev);
        assert_eq!(config.extra["future_field"], true);
    }
}
