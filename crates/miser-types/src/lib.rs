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

impl MessageContent {
    /// The text of this content, ignoring non-text parts.
    ///
    /// Lives here rather than at each call site so a second copy cannot drift
    /// out of step with the first: the classifier needs this to build its
    /// request envelope, and the corpus gates need it to name a case in a
    /// failure message.
    pub fn to_text(&self) -> String {
        match self {
            MessageContent::Text(text) => text.clone(),
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|part| match part {
                    ContentPart::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageUrl {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

/// OpenAI-compatible clients send `content: null` on assistant messages that
/// carry only `tool_calls`. `MessageContent` has no null variant, so axum's
/// `Json` extractor rejected the whole request with 422 before any routing
/// happened. Treat null and an absent `content` as empty text.
fn default_content() -> MessageContent {
    MessageContent::Text(String::new())
}

fn deserialize_content<'de, D>(deserializer: D) -> Result<MessageContent, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<serde_json::Value>::deserialize(deserializer)? {
        None | Some(serde_json::Value::Null) => Ok(default_content()),
        Some(value) => serde_json::from_value(value).map_err(serde::de::Error::custom),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default = "default_content", deserialize_with = "deserialize_content")]
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

/// What to do when the security screen fires.
///
/// `Tag` is the default because a classifier that silently drops traffic is
/// worse than one that reports it: the caller asked for routing help, not a
/// policy decision. `Refuse` exists for callers that have made that decision
/// themselves and want it enforced at the routing layer.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum SecurityAction {
    /// Record the risk, change nothing else.
    #[default]
    Tag,
    /// Record the risk and raise the tier, so the strongest model sees it.
    Escalate,
    /// Fail the classification outright.
    Refuse,
}

/// Prompt-injection and secret-extraction screening, asked as a Jev `noul`
/// alongside the tier question in the same request.
///
/// The screen is a *signal*, not a filter: the probability is recorded on the
/// result so the gateway can log, count and alert on it, and
/// `on_detect` decides what the router itself does about it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClassifierSecurityConfig {
    /// Ask the screening question at all. Off by default because it costs a
    /// question's worth of input tokens on every request.
    #[serde(default)]
    pub enabled: bool,
    /// The question asked of Jev. Left empty, the documented default is used.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instructions: String,
    /// Probability at or above which the screen is considered to have fired.
    #[serde(default = "default_security_threshold")]
    pub threshold: f32,
    #[serde(default, skip_serializing_if = "is_default_action")]
    pub on_detect: SecurityAction,
}

fn default_security_threshold() -> f32 {
    0.5
}

fn is_default_action(action: &SecurityAction) -> bool {
    *action == SecurityAction::default()
}

impl Default for ClassifierSecurityConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            instructions: String::new(),
            threshold: default_security_threshold(),
            on_detect: SecurityAction::default(),
        }
    }
}

/// What to do when verification says the cheap answer was wrong.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum CascadeAction {
    /// Raise the tier so the strongest available model sees it. This is the
    /// point of the cascade: a caught misroute costs one extra call, a missed
    /// one costs the whole answer being wrong.
    #[default]
    Escalate,
    /// Trust the cheap answer and record the disagreement. Cheaper, and the
    /// right setting when escalation is more expensive than the error.
    Accept,
}

/// Two-stage classification: decide locally, then pay to check the decision.
///
/// The local heuristic is free and instant but is a fixed pattern set, so it
/// fails in ways that are systematic rather than random -- the same phrasing
/// always misroutes. Verification asks Jev whether the tier that was chosen is
/// actually the right one, and escalates when it is not. This is the router
/// analogue of a verified cascade: the check is only worth paying for where the
/// cheap answer is unsure, so it is gated on confidence rather than run on
/// every request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClassifierCascadeConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Only verify when the local answer's confidence is at or below this.
    /// Verifying a decision the heuristic is confident about spends money to
    /// confirm something the patterns already got right.
    #[serde(default = "default_verify_below")]
    pub verify_below: f32,
    /// A verification failure raises the tier to at least Hard.
    #[serde(default, skip_serializing_if = "is_default_cascade")]
    pub on_unverified: CascadeAction,
    /// Confidence the verifier must reach before its disagreement is believed.
    /// A verifier that is itself unsure should not be able to escalate traffic.
    #[serde(default = "default_verify_confidence")]
    pub verify_confidence: f32,
}

fn default_verify_below() -> f32 {
    0.70
}

fn default_verify_confidence() -> f32 {
    0.80
}

fn is_default_cascade(action: &CascadeAction) -> bool {
    *action == CascadeAction::default()
}

impl Default for ClassifierCascadeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            verify_below: default_verify_below(),
            on_unverified: CascadeAction::default(),
            verify_confidence: default_verify_confidence(),
        }
    }
}

/// Per-million-token prices for the classifier itself, used to turn the
/// `usage` block into a number.
///
/// TypeSafe direct does not return `usage.cost` -- that field is an OpenRouter
/// billing addition -- so the cost of a routing decision has to be computed
/// locally or not at all.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClassifierCostConfig {
    #[serde(default)]
    pub enabled: bool,
    /// USD per 1M input tokens.
    #[serde(default)]
    pub price_in: f64,
    /// USD per 1M output tokens.
    #[serde(default)]
    pub price_out: f64,
}

impl Default for ClassifierCostConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            price_in: 0.0,
            price_out: 0.0,
        }
    }
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
    /// Probability that the security screen fired, when one was asked. `None`
    /// means "not screened", which is deliberately distinct from `Some(0.0)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security_risk: Option<f32>,
    /// The dated model snapshot that actually served the decision, e.g.
    /// `jev-1.13.0`. Recorded because thresholds tuned against one version
    /// silently stop meaning anything on the next.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jev_model: Option<String>,
    /// Local cost of the routing decision in USD, when cost accounting is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classifier_cost_usd: Option<f64>,
    /// How the tier was arrived at: `local`, `local-verified`, or
    /// `local-escalated` after a verifier disagreed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cascade: Option<String>,
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
    /// Two-stage classification: decide locally, then verify the decision.
    #[serde(default)]
    pub cascade: ClassifierCascadeConfig,
    /// Prompt-injection / secret-extraction screening, asked as a `noul`.
    #[serde(default)]
    pub security: ClassifierSecurityConfig,
    /// Local cost accounting for the classifier's own calls.
    #[serde(default)]
    pub cost: ClassifierCostConfig,
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
    /// Serve semantically-similar cached responses (embedding candidate +
    /// Jev equivalence validation). Requires a configured quality judge.
    #[serde(default)]
    pub semantic_enabled: bool,
    /// Embedding similarity above which a cached response becomes a
    /// validation candidate. The judge decides whether it is served.
    #[serde(default = "default_semantic_candidate_threshold")]
    pub semantic_candidate_threshold: f32,
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
            semantic_enabled: false,
            semantic_candidate_threshold: 0.75,
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
fn default_semantic_candidate_threshold() -> f32 {
    0.65
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
    use serde_json::json;

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

    #[test]
    fn assistant_tool_call_messages_round_trip() {
        let assistant = json!({
            "role": "assistant",
            "content": "Calling the shell.",
            "tool_calls": [{"id":"call_1","type":"function","function":{"name":"shell","arguments":"{\"command\":\"ls\"}"}}]
        });
        let message: ChatMessage = serde_json::from_value(assistant.clone()).unwrap();
        assert!(message.tool_calls.is_some());
        assert_eq!(serde_json::to_value(&message).unwrap(), assistant);

        let tool = json!({"role":"tool","content":"total 0","tool_call_id":"call_1"});
        let message: ChatMessage = serde_json::from_value(tool.clone()).unwrap();
        assert_eq!(message.tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(serde_json::to_value(&message).unwrap(), tool);
    }

    #[test]
    fn content_parts_decode_known_and_unknown_types() {
        let parts: Vec<ContentPart> = serde_json::from_value(json!([
            {"type":"text","text":"look"},
            {"type":"image_url","image_url":{"url":"https://x/img.png","detail":"high","dpi":144}},
            {"type":"refusal","refusal":"no"},
            {"type":"hologram","density":3}
        ]))
        .unwrap();
        assert!(matches!(parts[0], ContentPart::Text { .. }));
        match &parts[1] {
            ContentPart::ImageUrl { image_url } => {
                assert_eq!(image_url.url, "https://x/img.png");
                assert_eq!(image_url.detail.as_deref(), Some("high"));
                assert_eq!(image_url.extra["dpi"], 144);
            }
            other => panic!("unexpected part: {other:?}"),
        }
        assert!(matches!(parts[2], ContentPart::Refusal { .. }));
        // Parts the gateway does not know must not fail the client request.
        assert!(matches!(parts[3], ContentPart::Other));

        let text: MessageContent = serde_json::from_value(json!("hello")).unwrap();
        assert_eq!(text, MessageContent::Text("hello".into()));
        let parts: MessageContent =
            serde_json::from_value(json!([{"type":"text","text":"hi"}])).unwrap();
        assert!(matches!(parts, MessageContent::Parts(_)));
        assert_eq!(serde_json::to_value(&text).unwrap(), json!("hello"));
    }

    #[test]
    fn assistant_tool_call_message_with_null_content_decodes() {
        // OpenCode, Claude Code, and other OpenAI-compatible clients send
        // `content: null` when an assistant turn carries only tool_calls.
        // Before this was accepted, axum's Json extractor rejected the whole
        // request with 422 and the gateway never routed it.
        let raw = json!({
            "model": "auto",
            "messages": [
                {"role": "user", "content": "read it"},
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "read", "arguments": "{}"}
                    }]
                },
                {"role": "tool", "tool_call_id": "call_1", "content": "file body"}
            ]
        });
        let request: ChatCompletionRequest =
            serde_json::from_value(raw).expect("null content must decode");
        assert_eq!(request.messages.len(), 3);
        assert_eq!(
            request.messages[1].content,
            MessageContent::Text(String::new())
        );
        assert!(request.messages[1].tool_calls.is_some());
        assert_eq!(request.messages[2].role, "tool");

        // An absent content key is equally valid.
        let absent: ChatMessage = serde_json::from_value(json!({"role": "assistant"})).unwrap();
        assert_eq!(absent.content, MessageContent::Text(String::new()));
    }

    #[test]
    fn sampling_and_stream_fields_round_trip() {
        let raw = json!({
            "model": "m",
            "messages": [{"role":"user","content":"hi"}],
            "stream": true,
            "max_tokens": 256,
            "max_completion_tokens": 512,
            "stop": ["END"],
            "seed": 7,
            "user": "user-1",
            "logprobs": {"top_logprobs": 5}
        });
        let request: ChatCompletionRequest = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(request.stream, Some(true));
        assert_eq!(request.max_tokens, Some(256));
        assert_eq!(request.max_completion_tokens, Some(512));
        assert_eq!(serde_json::to_value(&request).unwrap(), raw);

        let with_floats: ChatCompletionRequest = serde_json::from_value(json!({
            "model": "m",
            "messages": [{"role":"user","content":"hi"}],
            "temperature": 0.2,
            "top_p": 0.9
        }))
        .unwrap();
        assert_eq!(with_floats.temperature, Some(0.2));
        assert_eq!(with_floats.top_p, Some(0.9));

        // Omitted optionals stay out of the serialized payload an upstream
        // sees.
        let minimal: ChatCompletionRequest = serde_json::from_value(
            json!({"model":"m","messages":[{"role":"user","content":"hi"}]}),
        )
        .unwrap();
        assert_eq!(minimal.stream, None);
        assert_eq!(
            serde_json::to_value(&minimal).unwrap(),
            json!({"model":"m","messages":[{"role":"user","content":"hi"}]})
        );
    }

    #[test]
    fn tiers_serialize_lowercase_for_config_compat() {
        assert_eq!(
            serde_json::from_str::<ComplexityTier>("\"reasoning\"").unwrap(),
            ComplexityTier::Reasoning
        );
        assert_eq!(
            serde_json::to_string(&ComplexityTier::Hard).unwrap(),
            "\"hard\""
        );
    }

    #[test]
    fn classification_result_round_trips_snake_case_enums() {
        let result = ClassificationResult {
            tier: ComplexityTier::Hard,
            confidence: 0.9,
            reasons: vec!["tools".into()],
            classifier: "jev".into(),
            latency_ms: 12,
            task: Some(TaskType::Coding),
            risk: Some(RiskLevel::High),
            privacy: Some(PrivacyLevel::Confidential),
            security_risk: Some(0.93),
            jev_model: Some("jev-1.13.0".into()),
            classifier_cost_usd: Some(0.000_020),
            cascade: Some("local-verified".into()),
            extra: Default::default(),
        };
        let raw = serde_json::to_value(&result).unwrap();
        assert_eq!(raw["task"], "coding");
        assert_eq!(raw["risk"], "high");
        assert_eq!(raw["privacy"], "confidential");
        // The screening probability and the serving version both survive the
        // wire, which is the point of recording them.
        // f32, so compare as f32 rather than against a literal.
        assert_eq!(raw["security_risk"].as_f64().unwrap() as f32, 0.93);
        assert_eq!(raw["jev_model"], "jev-1.13.0");
        assert_eq!(raw["classifier_cost_usd"], 0.000_020);
        assert_eq!(raw["cascade"], "local-verified");
        assert_eq!(
            serde_json::from_value::<ClassificationResult>(raw).unwrap(),
            result
        );
    }

    #[test]
    fn classifier_endpoint_config_covers_endpoint_extras() {
        let endpoint: ClassifierEndpointConfig = toml::from_str(
            r#"
            enabled = true
            model = "jev-latest"
            path = "/systemone"
            api_key = "k"
        "#,
        )
        .unwrap();
        assert!(endpoint.enabled);
        assert_eq!(endpoint.model, "jev-latest");
        assert_eq!(endpoint.path.as_deref(), Some("/systemone"));
        assert_eq!(endpoint.api_key.as_deref(), Some("k"));

        let empty: ClassifierEndpointConfig = toml::from_str("").unwrap();
        assert!(!empty.enabled);
        assert_eq!(empty.model, "");
        assert!(empty.path.is_none());
    }

    #[test]
    fn routing_filters_and_bands_carry_sane_defaults() {
        let filters = RoutingFilters::default();
        assert!(filters.require_tools);
        assert_eq!(filters.min_context, 65_536);
        assert!(!filters.allow_free);
        assert!(filters.deny_prefixes.is_empty());
        assert!(filters.prefer_reasoning_pin);
        assert_eq!(toml::from_str::<RoutingFilters>("").unwrap(), filters);

        let bands = RoutingBands::default();
        // Bands must ascend to partition the catalog into five tiers.
        assert!(bands.trivial_max < bands.simple_max);
        assert!(bands.simple_max < bands.standard_max);
        assert!(bands.standard_max < bands.reasoning_max);
        let partial: RoutingBands = toml::from_str("simple_max = 0.05").unwrap();
        assert_eq!(partial.simple_max, 0.05);
        assert_eq!(partial.trivial_max, bands.trivial_max);
        assert_eq!(partial.reasoning_max, bands.reasoning_max);
    }

    #[test]
    fn tier_route_config_round_trips_generation_params() {
        let raw = json!({
            "model": "openai/gpt-4.1-mini",
            "max_tokens": 1536,
            "provider": "openai",
            "max_cost_per_1m": {"prompt": 0.4, "completion": 1.6}
        });
        let route: TierModelRouteConfig = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(route.max_tokens, Some(1536));
        assert_eq!(route.temperature, None);
        assert_eq!(route.max_cost_per_1m.as_ref().unwrap().prompt, Some(0.4));
        assert_eq!(serde_json::to_value(&route).unwrap(), raw);

        let minimal: TierModelRouteConfig = serde_json::from_value(json!({"model":"m"})).unwrap();
        assert_eq!(minimal.max_tokens, None);
        assert_eq!(
            serde_json::to_value(&minimal).unwrap(),
            json!({"model":"m"})
        );

        let with_temperature: TierModelRouteConfig =
            serde_json::from_value(json!({"model":"m","temperature":0.3})).unwrap();
        assert_eq!(with_temperature.temperature, Some(0.3));
    }

    #[test]
    fn quality_and_cache_defaults_load_from_empty_config() {
        let quality: QualityConfig = toml::from_str("").unwrap();
        assert!(!quality.enabled);
        assert!((quality.minimum_score - 0.7).abs() < 1e-6);
        // Serde default (bool::default) — note the divergence from
        // QualityConfig::default(), which sets this to true.
        assert!(!quality.escalate_on_failure);
        assert!(quality.judge.is_none());

        let cache: CacheConfig = toml::from_str("").unwrap();
        assert!(cache.enabled);
        assert_eq!(cache.max_entries, 10_000);
        assert!((cache.similarity_threshold - 0.92).abs() < 1e-6);
        assert!(!cache.semantic_enabled);
        assert!((cache.semantic_candidate_threshold - 0.65).abs() < 1e-6);
        assert!(cache.embedding_model.is_none());
    }

    #[test]
    fn minimal_gateway_config_fills_host_port_and_routing_defaults() {
        let config: GatewayConfig = serde_json::from_value(json!({
            "classifier": {},
            "provider": {"api_key": ""},
            "tiers": {}
        }))
        .unwrap();
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.port, 8787);
        assert_eq!(config.routing.mode, RoutingMode::Fixed);
        assert!(config.tiers.is_empty());
    }

    #[test]
    fn catalog_snapshot_round_trips_tier_pins() {
        let snapshot = CatalogSnapshot {
            fetched_at: Some(1_700_000_000),
            source: "openrouter".into(),
            pins: [(
                ComplexityTier::Hard,
                TierPin {
                    model: "m".into(),
                    candidates: vec!["m".into(), "backup".into()],
                },
            )]
            .into_iter()
            .collect(),
            model_tiers: [("model/a".to_owned(), ComplexityTier::Simple)]
                .into_iter()
                .collect(),
            total_models: 2,
            unranked_models: 1,
        };
        let raw = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(raw["pins"]["hard"]["model"], "m");
        assert_eq!(
            serde_json::from_value::<CatalogSnapshot>(raw).unwrap(),
            snapshot
        );
    }
}
