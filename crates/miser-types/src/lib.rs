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
    #[serde(flatten)]
    pub extra: ExtraFields,
}

fn default_classifier_mode() -> ClassifierMode {
    ClassifierMode::Hybrid
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
    #[serde(default)]
    pub quality: QualityConfig,
    #[serde(default)]
    pub cache: CacheConfig,
    #[serde(default)]
    pub session: SessionConfig,
    #[serde(flatten)]
    pub extra: ExtraFields,
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
}
