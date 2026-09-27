use miser_types::{
    ChatCompletionRequest, ClassificationResult, ClassifierConfig, ClassifierMode, ComplexityTier,
    MessageContent, TaskType,
};
use regex::RegexSet;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Instant;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClassifierError {
    #[error("classifier request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("classifier returned invalid response: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("classifier endpoint is disabled or misconfigured")]
    Config,
    #[error("classifier response had an unexpected format")]
    Format,
}

#[derive(Clone)]
pub struct Classifier {
    config: ClassifierConfig,
    client: reqwest::Client,
    trivial: RegexSet,
    simple: RegexSet,
    standard: RegexSet,
    hard: RegexSet,
    reasoning: RegexSet,
}

#[derive(Debug, Deserialize)]
struct LlmResult {
    tier: ComplexityTier,
    #[serde(default = "default_confidence")]
    confidence: f32,
    #[serde(default)]
    reason: String,
}

fn default_confidence() -> f32 {
    0.7
}

impl Classifier {
    pub fn new(config: ClassifierConfig) -> Result<Self, regex::Error> {
        Ok(Self {
            config,
            client: reqwest::Client::builder()
                .build()
                .expect("client construction"),
            trivial: RegexSet::new([
                r"(?i)^\s*(hello|hi|hey|thanks|thank you|ok|okay|good morning|bye|please|help|version)\s*(?:there|everyone|all|team|folks)?\s*[!.]*\s*$",
                r"(?i)\b(git status|git diff|git log|v\d+\.\d+\.\d+)\b",
                r"(?i)^(what is|what's|how to|how do|how does|where is)\s+(your\s+name|2\s*\+\s*2|the\s+time|the\s+date|my\s+name)\b",
                r"(?i)\b(rename|uppercase|lowercase|trim|hello world|test input|unit test)\b.*\b(variable|file|string|line)\b",
                r"(?i)^\s*(yes|no|true|false)\s*[.!]?\s*$",
                r"(?i)\b(port|ip address|hostname|help|support)\b",
            ])?,
            simple: RegexSet::new([
                r"(?i)\b(explain|summarize|compare|convert|translate|format|describe|tell\s+me|demo|example|snippet|shell command|powershell)\b",
                r"(?i)\b(write|create)\s+(a|an)\s+(small|simple)?\s*\w*\s*(function|class|regex|script|interface)\b",
                r"(?i)\b(add|change|fix)\s+(a|the)\s+(comment|null check|format|timeout|max_tokens|default config)\b",
                r"(?i)\b(dockerfile|docker-compose|readme|migration|docker)\b",
                r"(?i)\b(sql|query|select|insert|index)\b.*\b(write|create|add|optimize)\b",
                r"(?i)\b(unit test|snapshot test|test for|powershell|bash)\b",
                r"(?i)\b(cors|semicolon|trailing|whitespace|quotes|tab|spaces)\b",
                r"(?i)\b(git command|curl command|shell command)\b",
                r"(?i)\b(type|interface|schema)\b.*\b(for|with)\b.*\b(id|name|email|field)\b",
                r"(?i)\b(dependency|package|install|import)\b.*\b(add|fix|update)\b",
                r"(?i)^(what is|what's|what are)\s+",
                r"(?i)\b(npm|yarn|pip|cargo)\b.*\b(what|how|explain|difference)\b",
                r"(?i)\b(ci.cd|pipeline|workflow)\b.*\b(what|how|explain|about|tell)\b",
            ])?,
            standard: RegexSet::new([
                r"(?i)\b(implement|build|integrate|debug|refactor|test|endpoint|migration|user management|authentication|database schema|notification)\b",
                r"(?i)\b(api|database|authentication|middleware|component|rate limiter|notification)\b.*\b(add|create|implement|design)\b",
                r"(?i)\b(rate limit|jwt|oauth|redis|queue|webhook|middleware|pagination)\b",
                r"(?i)\b(kubernetes|terraform|ansible|istio|prometheus|grafana)\b",
                r"(?i)\b(react|useeffect|memo|bundle|webpack)\b.*\b(optimize|fix|implement)\b",
                r"(?i)\b(github actions|ci.cd|pipeline|workflow)\b",
                r"(?i)\b(property.based|integration test|load test|contract test|pact)\b",
                r"(?i)\b(encrypt|decrypt|csp|cors|jwt|token)\b.*\b(implement|add|configure)\b",
                r"(?i)\b(trie|bloom filter|lru cache|dijkstra|merge sort|token bucket)\b",
                r"(?i)\b(memoiz|snapshot|batch|dataload|connection pool)\b",
            ])?,
            hard: RegexSet::new([
                r"(?i)\b(architect|distributed|production incident|threat-model|zero-downtime|multi-region|multi-region)\b",
                r"(?i)\b(security|concurrency|race condition|migration|rollout|failover)\b.*\b(design|analy[sz]e|plan|fix)\b",
                r"(?i)\b(one million|40 services|80-file|across (all|every|five))\b",
                r"(?i)\b(service mesh|istio|mtls|saml|sso|graphql resolver)\b",
                r"(?i)\b(end.to.end encryption|signal protocol|x3dh)\b",
                r"(?i)\b(event sourcing|consistent hashing|skip list|crdt)\b",
                r"(?i)\b(chaos engineering|mutation test|deadlock|slo|rto|rpo)\b",
                r"(?i)\b(design|architect)\b.*\b(url shortener|notification|scheduler|search|payment|chat|gateway)\b",
                r"(?i)\b(production|incident|outage|postmortem)\b.*\b(analy[sz]e|investigate|debug)\b",
                r"(?i)\b(secrets management|vault|pci.dss|field.level encryption)\b",
            ])?,
            reasoning: RegexSet::new([
                r"(?i)\b(prove|derive|counterexample|formal|satisfiable|optimality|correctness)\b",
                r"(?i)\b(algorithm|recurrence|serialization graph|posterior|inference)\b.*\b(analysis|design|prove|derive|bound)\b",
                r"(?i)\b(amortized|invariant|converge|distributed counter)\b.*\b(prove|derive|analysis)\b",
                r"(?i)\b(halting problem|undecidable|diagonal)\b",
                r"(?i)\b(reduction|3.sat|polynomial.time|complexity class)\b",
                r"(?i)\b(bayesian|posterior|conjugate|likelihood)\b.*\b(derive|prove|estimate)\b",
            ])?,
        })
    }

    /// Stable string form of the configured mode, used by eval tooling to
    /// attribute per-case results.
    pub fn mode_name(&self) -> &'static str {
        match self.config.mode {
            ClassifierMode::Heuristic => "heuristic",
            ClassifierMode::LocalLlm => "local_llm",
            ClassifierMode::CloudLlm => "cloud_llm",
            ClassifierMode::Jev => "jev",
            ClassifierMode::Hybrid => "hybrid",
        }
    }

    pub async fn classify(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<ClassificationResult, ClassifierError> {
        let started = Instant::now();
        let text = request_text(request);
        if let Some((tier, reason)) = override_tier(request) {
            return Ok(result(
                tier,
                1.0,
                "override",
                vec![reason],
                started,
                task(&text),
            ));
        }

        let heuristic = self.heuristic(&text, request, started);
        match self.config.mode {
            ClassifierMode::Heuristic => Ok(heuristic),
            ClassifierMode::LocalLlm => self
                .llm(request, &self.config.local_llm, "local_llm", started)
                .await
                .or(Ok(heuristic)),
            ClassifierMode::CloudLlm => self
                .llm(request, &self.config.cloud_llm, "cloud_llm", started)
                .await
                .or(Ok(heuristic)),
            ClassifierMode::Jev => match self.jev(request, started).await {
                Ok(result) => Ok(result),
                Err(error) => {
                    // Fallback is intentional (availability over accuracy) but
                    // must be observable: under mode = "jev" this header flip
                    // is the only signal of a Jev outage.
                    tracing::warn!(error = %error, "jev classification failed; falling back to heuristic");
                    Ok(heuristic)
                }
            },
            ClassifierMode::Hybrid => {
                if heuristic.confidence >= self.config.confidence_threshold {
                    return Ok(heuristic);
                }
                let local_fut = if self.config.local_llm.enabled {
                    Some(Box::pin(self.llm(
                        request,
                        &self.config.local_llm,
                        "local_llm",
                        started,
                    )))
                } else {
                    None
                };
                let cloud_fut = if self.config.cloud_llm.enabled {
                    Some(Box::pin(self.llm(
                        request,
                        &self.config.cloud_llm,
                        "cloud_llm",
                        started,
                    )))
                } else {
                    None
                };
                match (local_fut, cloud_fut) {
                    (Some(local), Some(cloud)) => {
                        let mut local = local;
                        let mut cloud = cloud;
                        tokio::select! {
                            result = &mut local => match result {
                                Ok(r) if r.confidence >= self.config.confidence_threshold => Ok(r),
                                Ok(local_result) => {
                                    match cloud.as_mut().await {
                                        Ok(cloud_result) if cloud_result.confidence >= self.config.confidence_threshold => Ok(cloud_result),
                                        Ok(_) => Ok(local_result),
                                        Err(_) => Ok(local_result),
                                    }
                                }
                                Err(_) => match cloud.as_mut().await {
                                    Ok(r) => Ok(r),
                                    Err(_) => Ok(heuristic),
                                },
                            },
                            result = &mut cloud => match result {
                                Ok(r) if r.confidence >= self.config.confidence_threshold => Ok(r),
                                Ok(cloud_result) => match local.as_mut().await {
                                    Ok(local_result) if local_result.confidence >= self.config.confidence_threshold => Ok(local_result),
                                    Ok(_) => Ok(cloud_result),
                                    Err(_) => Ok(cloud_result),
                                },
                                Err(_) => match local.as_mut().await {
                                    Ok(r) => Ok(r),
                                    Err(_) => Ok(heuristic),
                                },
                            },
                        }
                    }
                    (Some(mut local), None) => local.as_mut().await.or(Ok(heuristic)),
                    (None, Some(mut cloud)) => cloud.as_mut().await.or(Ok(heuristic)),
                    (None, None) => Ok(heuristic),
                }
            }
        }
    }

    fn heuristic(
        &self,
        text: &str,
        request: &ChatCompletionRequest,
        started: Instant,
    ) -> ClassificationResult {
        let classification_task = task(text);
        let mut reasons = Vec::new();
        let mut scores = [
            (ComplexityTier::Trivial, 0_i32),
            (ComplexityTier::Simple, 1),
            (ComplexityTier::Standard, 0),
            (ComplexityTier::Hard, 0),
            (ComplexityTier::Reasoning, 0),
        ];
        let sets = [
            (&self.trivial, 0, 5),
            (&self.simple, 1, 3),
            (&self.standard, 2, 4),
            (&self.hard, 3, 6),
            (&self.reasoning, 4, 7),
        ];
        let mut trivial_matches = 0;
        for (set, index, weight) in sets {
            let matches = set.matches(text).into_iter().count() as i32;
            scores[index].1 += matches * weight;
            if index == 0 {
                trivial_matches = matches;
            }
            if matches > 0 {
                reasons.push(format!("pattern:{}:{}", index, matches));
            }
        }
        if request.tools.as_ref().is_some_and(|x| !x.is_empty()) {
            scores[2].1 += 4;
            reasons.push("tools-present".into());
        }
        if request.messages.len() > 10 {
            scores[2].1 += 3;
            reasons.push("deep-conversation".into());
        }
        if request.response_format.is_some() {
            scores[2].1 += 2;
            reasons.push("structured-output".into());
        }
        if has_explanatory_context(text) && scores[0].1 == 0 {
            scores[1].1 += 5;
            reasons.push("explanatory-context".into());
        }
        // A detected coding task is a strong signal, but it must not outrank a
        // tier the *text itself* matched. The bonus is 10 while a single
        // Hard pattern is 6 and a single Reasoning pattern 7, so
        // "Prove correctness of the CRDT implementation" scored Reasoning 7 and
        // then lost to Standard 11 -- a formal-correctness proof served by the
        // mid-tier model. The floor at Standard is kept; the bonus is only
        // withheld when a higher tier already has a pattern match, which is
        // exactly the "when two tiers are plausible choose the higher one"
        // rule the Jev prompt states.
        if has_light_agentic(text) && trivial_matches == 0 {
            // A lookup the Trivial table does not list is an operational query
            // against a live target -- "the deployment", "the database", "the
            // src directory" -- not a bare command. The Trivial table covers the
            // bare form (`git status`, `git diff`, `git log`) and the tier
            // patterns settle those on their own.
            //
            // This used to ride in on the +10 `coding-task` bonus, which made a
            // status check look like software engineering. Routing it separately
            // is what keeps the bare form at Trivial *and* puts the operational
            // form on the mid tier, instead of trading one mistake for the
            // other.
            scores[2].1 += 10;
            reasons.push("operational-lookup".into());
        }
        if classification_task == Some(TaskType::Coding)
            && scores[1].1 <= 1
            && scores[3].1 == 0
            && scores[4].1 == 0
        {
            scores[2].1 += 10;
            reasons.push("coding-task".into());
        }
        if classification_task == Some(TaskType::Agentic) {
            scores[3].1 += 15;
            reasons.push("agentic-task".into());
        }
        if has_agentic_tools(request) {
            scores[3].1 += 12;
            reasons.push("agentic-tools".into());
        }
        if has_tool_history(request) {
            scores[3].1 += 20;
            reasons.push("tool-history".into());
        }
        if has_multi_step_intent(text) {
            scores[3].1 += 8;
            reasons.push("multi-step-intent".into());
        }
        let last = scores
            .iter()
            .max_by_key(|(_, score)| *score)
            .copied()
            .unwrap_or((ComplexityTier::Standard, 0));
        let confidence = if last.1 <= 1 {
            0.5
        } else {
            (0.55 + last.1 as f32 / 30.0).min(0.95)
        };
        result(
            last.0,
            confidence,
            "heuristic",
            reasons,
            started,
            classification_task,
        )
    }

    async fn llm(
        &self,
        request: &ChatCompletionRequest,
        endpoint: &miser_types::ClassifierEndpointConfig,
        name: &str,
        started: Instant,
    ) -> Result<ClassificationResult, ClassifierError> {
        if !endpoint.enabled || endpoint.base_url.is_empty() || endpoint.model.is_empty() {
            return Err(ClassifierError::Config);
        }
        let body = json!({ "model": endpoint.model, "messages": [{"role":"system","content":"Classify minimum required capability. Return only JSON {tier: trivial|simple|standard|hard|reasoning, confidence: number, reason: string}. Judge work required, not keywords."},{"role":"user","content":request_text(request)}], "temperature":0, "max_tokens":180, "think":false, "response_format":{"type":"json_object"} });
        let mut req = self
            .client
            .post(format!(
                "{}/chat/completions",
                endpoint.base_url.trim_end_matches('/')
            ))
            .timeout(std::time::Duration::from_millis(endpoint.timeout_ms))
            .json(&body);
        if let Some(key) = &endpoint.api_key {
            if !key.is_empty() {
                req = req.bearer_auth(key);
            }
        }
        let payload: serde_json::Value = req.send().await?.error_for_status()?.json().await?;
        let content = payload["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default();
        // The prompt asks for JSON, but models wrap it in a ```json fence often
        // enough that ignoring the fence means silently discarding a good
        // answer. `trim_matches('`')` only stripped backticks from the very
        // ends, so the common form became `json\n{...}\n` and both attempts
        // failed. Strip a leading fence line and its language tag, and the
        // trailing fence, then parse.
        let parsed: LlmResult = serde_json::from_str(content)
            .or_else(|_| serde_json::from_str(strip_code_fence(content).trim()))
            .or_else(|_| serde_json::from_str(content.trim_matches('`').trim()))?;
        Ok(result(
            parsed.tier,
            parsed.confidence.clamp(0.0, 0.99),
            name,
            vec![parsed.reason],
            started,
            task(&request_text(request)),
        ))
    }

    /// Classify through the Jev evaluation API (`POST {base_url}/evaluate`).
    ///
    /// Jev is a System One evaluation model, not a chat model: it receives a
    /// shared state plus typed questions and returns choices with
    /// probabilities. Tier and task are answered in one call; the tier
    /// probability is used directly as classification confidence.
    async fn jev(
        &self,
        request: &ChatCompletionRequest,
        started: Instant,
    ) -> Result<ClassificationResult, ClassifierError> {
        let endpoint = &self.config.jev;
        if !endpoint.enabled || endpoint.base_url.is_empty() || endpoint.model.is_empty() {
            return Err(ClassifierError::Config);
        }
        if endpoint.api_key.as_deref().unwrap_or_default().is_empty() {
            return Err(ClassifierError::Config);
        }
        // Tool context is part of the request envelope, not the prompt text:
        // without it Jev cannot apply the agentic capability floors.
        let tool_names: Vec<String> = request
            .tools
            .as_ref()
            .map(|tools| {
                tools
                    .iter()
                    .map(|tool| {
                        tool["function"]["name"]
                            .as_str()
                            .or_else(|| tool["name"].as_str())
                            .or_else(|| tool["type"].as_str())
                            .unwrap_or("unknown")
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let body = json!({
            "model": endpoint.model,
            "state": {
                "request": request_text(request),
                "tools": tool_names,
                "tool_history": has_tool_history(request)
            },
            "questions": {
                "tier": {
                    "type": "choice",
                    "instructions": "Classify the minimum capability tier required to answer this request. Judge the work and model capability required, never keywords: technical jargon in a trivial request stays trivial, and closing or small-talk messages stay trivial regardless of which technologies they mention. Decide from the inside out: is the answer one bare fact (date, arithmetic, protocol constant, yes/no) with no concept explanation? Then trivial. Asked to explain a concept, produce an artifact (snippet, regex, query, command, translation), or make a small edit? Simple, even when brief. Multi-file feature work, debugging, or infrastructure configuration? Standard. Designing or analyzing a system at scale, production incidents, security threat models, or cross-service migrations? Hard. Formal proof or derivation? Reasoning. Tool context overrides the text alone: when tools are attached or tool history exists, executing one read-only lookup is standard, and mutating or multi-step operations (install, deploy, migrate, commit) are hard. When two tiers are plausible, choose the higher one: under-routing demanding work to a weak model costs more than over-routing.",
                    "criteria": {
                        "trivial": "Greetings, thanks, and conversation-closing small talk, pure yes/no answers, bare facts like dates or port numbers, one-command lookups without tools, and tiny mechanical text edits such as rename or uppercase - even if they name complex technology",
                        "simple": "Explaining a concept even briefly, writing a snippet, regex, query, command, or single test, translating or summarizing text, single-file changes",
                        "standard": "Implementing features, debugging failures, multi-file or multi-component changes, API/database/schema work, CI or infrastructure configuration with substance, or single read-only tool operations",
                        "hard": "Architecture or system design at scale (high traffic, many services, many regions), production incident analysis, threat modeling, distributed transactions or migrations, large cross-service refactors, or mutating or multi-step tool operations",
                        "reasoning": "Formal proofs, complexity or algorithm analysis, derivations, correctness or optimality arguments"
                    }
                },
                "task": {
                    "type": "choice",
                    "instructions": "Which category best describes this request?",
                    "criteria": {
                        "chat": "Conversational or informational exchange",
                        "coding": "Writing, changing, or debugging software",
                        "reasoning": "Formal analysis, proofs, or derivations",
                        "agentic": "Executing actions, tools, or multi-step operations"
                    }
                }
            }
        });
        let path = match endpoint.path.as_deref() {
            Some("") | None => "/evaluate",
            Some(p) if p.starts_with("http://") || p.starts_with("https://") => p,
            Some(p) if p.starts_with('/') => p,
            Some(p) => {
                // Tolerate a missing leading slash rather than silently
                // mis-joining onto the base URL.
                &format!("/{p}")
            }
        };
        let url = if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else {
            format!("{}{}", endpoint.base_url.trim_end_matches('/'), path)
        };
        let req = self
            .client
            .post(url)
            .timeout(std::time::Duration::from_millis(endpoint.timeout_ms))
            .json(&body)
            .bearer_auth(endpoint.api_key.as_deref().unwrap_or_default());
        // A structured endpoint should return bare JSON, but a gateway or proxy
        // in front of it can hand back a fenced or prose-wrapped body, and the
        // LLM path already tolerates that. Two paths reading the same contract
        // differently is how a fix to one silently misses the other, so recover
        // the same way here and keep the availability-over-accuracy policy.
        let raw = req.send().await?.error_for_status()?.text().await?;
        let payload: serde_json::Value = serde_json::from_str(raw.trim())
            .or_else(|_| serde_json::from_str(strip_code_fence(&raw).trim()))
            .map_err(|_| ClassifierError::Format)?;
        let answers = &payload["answers"];
        let tier_str = answers["tier"]["choice"].as_str().unwrap_or_default();
        let tier = match tier_str {
            "trivial" => ComplexityTier::Trivial,
            "simple" => ComplexityTier::Simple,
            "standard" => ComplexityTier::Standard,
            "hard" => ComplexityTier::Hard,
            "reasoning" => ComplexityTier::Reasoning,
            _ => return Err(ClassifierError::Format),
        };
        // TypeSafe direct returns a per-answer `confidence`; the Gateway
        // contract only carries `probabilities`. Prefer the explicit field.
        // TypeSafe direct returns a per-answer `confidence`; the Gateway
        // contract only carries `probabilities`. Prefer the explicit field.
        //
        // A response carrying *neither* is not a confident answer, and must not
        // be laundered into one. This previously fell back to
        // `default_confidence()` = 0.70, which is above the 0.65 threshold in
        // the shipped config, and `ClassifierMode::Jev` applies no threshold of
        // its own -- so a payload with a bare `{"choice":"hard"}`, a `null`
        // confidence, a stringified `"0.95"`, or a `probabilities` map missing
        // the chosen key all came back as a confident Hard. The point of
        // separating the choice from its probability is lost, and the heuristic
        // that `classify` would have used as a fallback never gets a look in.
        // Treated as a format error instead, so the documented
        // "availability over accuracy" fallback applies.
        let confidence = match answers["tier"]["confidence"].as_f64().or_else(|| {
            answers["tier"]["probabilities"]
                .get(tier_str)
                .and_then(Value::as_f64)
        }) {
            Some(p) => (p as f32).clamp(0.0, 0.99),
            None => return Err(ClassifierError::Format),
        };
        let task_type = answers["task"]["choice"]
            .as_str()
            .and_then(|s| serde_json::from_value::<TaskType>(json!(s)).ok());
        let mut classification = result(
            tier,
            confidence,
            "jev",
            vec![format!("jev:{}", tier_str)],
            started,
            task_type,
        );
        if !payload["usage"].is_null() {
            classification
                .extra
                .insert("usage".into(), payload["usage"].clone());
        }
        Ok(classification)
    }
}

fn request_text(request: &ChatCompletionRequest) -> String {
    request
        .messages
        .iter()
        .map(|message| match &message.content {
            MessageContent::Text(text) => text.clone(),
            MessageContent::Parts(parts) => parts
                .iter()
                .map(|part| match part {
                    miser_types::ContentPart::Text { text } => text.clone(),
                    _ => String::new(),
                })
                .collect::<Vec<_>>()
                .join(" "),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Extract the contents of the first Markdown code fence, if there is one.
///
/// Handles a fence at the start of the text or after a prose preamble, with or
/// without a language tag on the opening line, and returns the input unchanged
/// when there is no fence. Matching the *first* closing fence rather than the
/// last matters when the trailing prose itself contains backticks.
fn strip_code_fence(content: &str) -> &str {
    let trimmed = content.trim();
    let Some(open) = trimmed.find("```") else {
        return trimmed;
    };
    // Skip past the opening fence and any language tag on the same line.
    let after_open = match trimmed[open..].find('\n') {
        Some(offset) => &trimmed[open + offset + 1..],
        None => return "",
    };
    match after_open.find("```") {
        Some(end) => after_open[..end].trim(),
        None => after_open.trim(),
    }
}
/// Text of a single message, ignoring non-text parts.
fn message_text(message: &miser_types::ChatMessage) -> String {
    message.content.to_text()
}

fn override_tier(request: &miser_types::ChatCompletionRequest) -> Option<(ComplexityTier, String)> {
    // The directive belongs to the user, not to whatever happens to open the
    // transcript. Reading it from the joined text meant any request starting
    // with a system prompt -- or a null-content assistant turn, which is every
    // tool-calling turn -- silently lost the override.
    let first_user = request.messages.iter().find(|m| m.role == "user")?;
    let text = message_text(first_user);
    let first = text.lines().next()?.trim();
    let directive = first.strip_prefix("@route:").or_else(|| {
        // Case-insensitive, like every other pattern in this file. Accepting
        // only exact lowercase meant `@route:Hard` was ignored without a word.
        if first.len() < 7 || !first[..7].eq_ignore_ascii_case("@route:") {
            return None;
        }
        Some(&first[7..])
    })?;
    let tier = directive.trim();
    let parsed = match tier.to_ascii_lowercase().as_str() {
        "trivial" => ComplexityTier::Trivial,
        "simple" => ComplexityTier::Simple,
        "standard" => ComplexityTier::Standard,
        "hard" => ComplexityTier::Hard,
        "reasoning" => ComplexityTier::Reasoning,
        _ => {
            // Failing open into the heuristic is the risky direction for an
            // explicit request, so say so rather than dropping it silently.
            tracing::warn!(
                directive = tier,
                "unknown @route: tier; ignoring the override"
            );
            return None;
        }
    };
    Some((parsed, format!("override:{tier}")))
}

fn task(text: &str) -> Option<TaskType> {
    let lower = text.to_lowercase();
    if has_explanatory_context(&lower) {
        return coding_or_reasoning(&lower);
    }
    if has_action_agentic(&lower) {
        return Some(TaskType::Agentic);
    }
    if has_light_agentic(&lower) {
        // Deliberately *not* a Coding task. This overlaps the Trivial tier
        // table -- `git status`, `git diff`, `git log` are listed there -- so
        // returning `Coding` earned the +10 `coding-task` bonus and overrode the
        // Trivial pattern that had just matched, putting every one-liner
        // `git status` in a session on the mid-tier model. A status check is a
        // lookup, not a coding task. Note this set is broader than the Trivial
        // table (it also matches prose like "print the version"), so returning
        // `None` leaves those to the tier patterns alone rather than demoting
        // them.
        return None;
    }
    coding_or_reasoning(&lower)
}

/// Whether `text` contains `needle` as a *word*.
///
/// These are unanchored substring checks because they look at words, and
/// substring matching on words silently matches other words that contain them:
/// `contains("code")` fires on "unicode" and "barcode", `contains("api")` on
/// "capital", `contains("rest")` on "interest" and "restaurant". Each false
/// positive mints a `Coding` task, which then earns the +10 bonus below and can
/// outvote a genuine Hard or Reasoning match -- so "The capital of France" was
/// classified as a coding task and escalated.
fn has_word(lower: &str, needle: &str) -> bool {
    lower.match_indices(needle).any(|(at, _)| {
        let before = lower[..at].chars().next_back();
        let after = lower[at + needle.len()..].chars().next();
        let boundary = |c: Option<char>| !c.is_some_and(char::is_alphanumeric);
        boundary(before) && boundary(after)
    })
}

fn coding_or_reasoning(lower: &str) -> Option<TaskType> {
    const CODING: [&str; 11] = [
        "code",
        "implement",
        "function",
        "python",
        "typescript",
        "debug",
        "api",
        "endpoint",
        "retry",
        "bug",
        "rest",
    ];
    if CODING.iter().any(|needle| has_word(lower, needle)) {
        Some(TaskType::Coding)
    } else if lower.contains("prove") || lower.contains("derive") || lower.contains("algorithm") {
        Some(TaskType::Reasoning)
    } else {
        Some(TaskType::Chat)
    }
}

fn has_explanatory_context(lower: &str) -> bool {
    static EXPLANATORY: std::sync::OnceLock<RegexSet> = std::sync::OnceLock::new();
    let regex = EXPLANATORY.get_or_init(|| {
        RegexSet::new([
            r"(?i)^(explain|what is|what's|what are|how to|how do|how does|describe|tell me|show me how|why|difference between)\b",
            r"(?i)\b(explain|describe)\b.*\b(how|what|why)\b",
            r"(?i)\b(what is|what's|what are)\b",
            r"(?i)\b(write|create)\s+(a|an|the)?\s*(unit\s+test|test|snapshot|integration\s+test)\b",
        ])
        .expect("explanatory regex")
    });
    regex.is_match(lower)
}

fn has_action_agentic(lower: &str) -> bool {
    static ACTION: std::sync::OnceLock<RegexSet> = std::sync::OnceLock::new();
    let regex = ACTION.get_or_init(|| {
        RegexSet::new([
            r"(?i)^\s*(run|execute|deploy|install|start|stop|restart|migrate|seed|scaffold|init|commit|push|publish)\b",
            r"(?i)\b(run|execute)\s+(the\s+)?(test|build|command|script|migration|server|service|app|application|suite|pipeline|linter|lint)\b",
            r"(?i)\b(deploy|publish|push)\s+(to|the)\b",
            r"(?i)\b(install|uninstall)\s+(the\s+)?(dependencies|deps|packages|package)\b",
            r"(?i)\b(start|stop|restart)\s+(the\s+)?(server|service|app|database|proxy)\b",
            r"(?i)\b(migrate|seed)\s+(the\s+)?(database|db)\b",
            r"(?i)\b(npm|yarn|cargo|pip|docker|kubectl|terraform|ansible|make)\s+(run|test|build|install|deploy|exec|apply|playbook|start|stop)\b",
            r"(?i)\bgit\s+(commit|push|pull|checkout|clone|merge|rebase)\b",
            r"(?i)\b(build|rebuild)\s+(the\s+)?(project|image|docker|binary|app|application)\b",
            r"(?i)\b(create|write|edit|delete|remove)\s+(a\s+|the\s+|new\s+)*file\b",
            r"(?i)\b(run|execute)\s+(npm|yarn|cargo|pip|docker|kubectl|terraform|ansible|make)\b",
            r"(?i)\bagent\b",
            r"(?i)\b(deploy|ship|release)\s+(to\s+)?(production|staging|prod)\b",
            r"(?i)\b(run|execute)\s+.+\s+and\s+(fix|report|show|deploy|push|commit|verify)\b",
            r"(?i)\b(build|test).+\band\s+(push|deploy|publish|ship|release)\b",
            r"(?i)\b(migrate).+\band\s+(seed|rollback|verify)\b",
        ])
        .expect("action-agentic regex")
    });
    regex.is_match(lower)
}

fn has_light_agentic(lower: &str) -> bool {
    static LIGHT: std::sync::OnceLock<RegexSet> = std::sync::OnceLock::new();
    let regex = LIGHT.get_or_init(|| {
        RegexSet::new([
            r"(?i)\b(check|show|display|list|read|view|get|print)\s+(the\s+)?(status|output|result|logs|files|config|version|diff|tree)\b",
            r"(?i)\b(git\s+status|git\s+log|git\s+diff|git\s+branch|git\s+show)\b",
            r"(?i)\b(list|show)\s+(the\s+)?(files|directories|services|containers|pods)\b",
            r"(?i)\b(read|cat|head|tail|less|more)\s+(a\s+|the\s+)?file\b",
            r"(?i)\b(check|verify|inspect|examine)\s+(if|whether|that|the)\b",
        ])
        .expect("light-agentic regex")
    });
    regex.is_match(lower)
}

fn has_agentic_tools(request: &ChatCompletionRequest) -> bool {
    // Word-matched, not substring-matched. `contains("file")` fired on
    // `get_user_profile` and `contains("search")` on `research_agent`, so a
    // plain read-only profile lookup put a "hello" on the Hard tier and the
    // reasoning model.
    const AGENTIC: [&str; 9] = [
        "shell", "bash", "execute", "run", "file", "search", "grep", "command", "terminal",
    ];
    request.tools.as_ref().is_some_and(|tools| {
        tools.iter().any(|tool| {
            let lowered = tool.to_string().to_lowercase();
            AGENTIC.iter().any(|needle| has_word(&lowered, needle))
        })
    })
}

fn has_tool_history(request: &ChatCompletionRequest) -> bool {
    request
        .messages
        .iter()
        .any(|msg| msg.tool_calls.is_some() || msg.tool_call_id.is_some() || msg.role == "tool")
}

fn has_multi_step_intent(text: &str) -> bool {
    static MULTI_STEP: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let regex = MULTI_STEP.get_or_init(|| {
        regex::Regex::new(r"(?i)\b\w+\s+.+\s+and\s+(fix|report|show|deploy|push|commit|verify|seed|rollback|publish|ship|release|install|start|stop|restart|configure|create|delete|edit|update)\b")
            .expect("multi-step regex")
    });
    regex.is_match(text)
}

fn result(
    tier: ComplexityTier,
    confidence: f32,
    classifier: &str,
    reasons: Vec<String>,
    started: Instant,
    task: Option<TaskType>,
) -> ClassificationResult {
    ClassificationResult {
        tier,
        confidence,
        reasons,
        classifier: classifier.into(),
        latency_ms: started.elapsed().as_millis() as u64,
        task,
        risk: None,
        privacy: None,
        extra: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miser_types::ChatCompletionRequest;

    /// A config that never calls out, for the tier rules themselves.
    fn classifier_config(mode: &str) -> ClassifierConfig {
        let mode = match mode {
            "heuristic" => ClassifierMode::Heuristic,
            "local_llm" => ClassifierMode::LocalLlm,
            other => panic!("no such classifier mode: {other:?}"),
        };
        let mut config: ClassifierConfig = serde_json::from_str("{}").unwrap();
        config.mode = mode;
        config.confidence_threshold = 0.65;
        config
    }

    /// A chat-completions endpoint pointed at the mock server.
    fn llm_config(base_url: &str) -> ClassifierConfig {
        let mut config = classifier_config("local_llm");
        config.local_llm.enabled = true;
        config.local_llm.model = "test-model".into();
        config.local_llm.base_url = base_url.into();
        config.local_llm.api_key = Some("test-key".into());
        config.local_llm.timeout_ms = 2000;
        config
    }

    /// Serve `body` once, for a test that needs the answer, not the request.
    async fn spawn_answer(
        body: serde_json::Value,
    ) -> (
        String,
        Arc<Mutex<Vec<MockRequest>>>,
        tokio::task::JoinHandle<()>,
    ) {
        spawn_mock(vec![MockHttpResponse {
            status: 200,
            body,
            delay_ms: 0,
        }])
        .await
    }

    fn request(text: &str) -> ChatCompletionRequest {
        serde_json::from_value(json!({"model":"auto","messages":[{"role":"user","content":text}]}))
            .unwrap()
    }

    #[tokio::test]
    async fn classifies_representative_tiers() {
        let mut config = ClassifierConfig {
            mode: ClassifierMode::Heuristic,
            ..serde_json::from_str("{}").unwrap()
        };
        config.confidence_threshold = 0.7;
        let classifier = Classifier::new(config).unwrap();
        assert_eq!(
            classifier.classify(&request("Hello")).await.unwrap().tier,
            ComplexityTier::Trivial
        );
        assert_eq!(
            classifier
                .classify(&request("Explain DNS"))
                .await
                .unwrap()
                .tier,
            ComplexityTier::Simple
        );
        assert_eq!(
            classifier
                .classify(&request("Implement an API endpoint"))
                .await
                .unwrap()
                .tier,
            ComplexityTier::Standard
        );
        assert_eq!(
            classifier
                .classify(&request("Architect a distributed cache"))
                .await
                .unwrap()
                .tier,
            ComplexityTier::Hard
        );
        assert_eq!(
            classifier
                .classify(&request("Prove this algorithm is optimal"))
                .await
                .unwrap()
                .tier,
            ComplexityTier::Reasoning
        );
    }

    #[tokio::test]
    async fn valid_override_wins() {
        let config = ClassifierConfig {
            mode: ClassifierMode::Heuristic,
            ..serde_json::from_str("{}").unwrap()
        };
        let classifier = Classifier::new(config).unwrap();
        assert_eq!(
            classifier
                .classify(&request("@route:trivial\nProve a theorem"))
                .await
                .unwrap()
                .tier,
            ComplexityTier::Trivial
        );
    }

    #[tokio::test]
    async fn agentic_keywords_route_to_hard() {
        let mut config = ClassifierConfig {
            mode: ClassifierMode::Heuristic,
            ..serde_json::from_str("{}").unwrap()
        };
        config.confidence_threshold = 0.7;
        let classifier = Classifier::new(config).unwrap();
        assert_eq!(
            classifier
                .classify(&request("Run the test suite and report results"))
                .await
                .unwrap()
                .tier,
            ComplexityTier::Hard
        );
        assert_eq!(
            classifier
                .classify(&request("Execute the build pipeline"))
                .await
                .unwrap()
                .tier,
            ComplexityTier::Hard
        );
        assert_eq!(
            classifier
                .classify(&request("Deploy the service to production"))
                .await
                .unwrap()
                .tier,
            ComplexityTier::Hard
        );
    }

    #[tokio::test]
    async fn agentic_tools_route_to_hard() {
        let config = ClassifierConfig {
            mode: ClassifierMode::Heuristic,
            ..serde_json::from_str("{}").unwrap()
        };
        let classifier = Classifier::new(config).unwrap();
        let req: ChatCompletionRequest = serde_json::from_value(json!({
            "model":"auto",
            "messages":[{"role":"user","content":"run the test suite"}],
            "tools":[{"type":"function","function":{"name":"shell","description":"Run a shell command"}}]
        }))
        .unwrap();
        let result = classifier.classify(&req).await.unwrap();
        assert!(
            result.tier >= ComplexityTier::Hard,
            "agentic tools with action intent should floor to at least Hard, got {:?}",
            result.tier
        );
    }

    #[tokio::test]
    async fn tool_history_routes_to_hard() {
        let config = ClassifierConfig {
            mode: ClassifierMode::Heuristic,
            ..serde_json::from_str("{}").unwrap()
        };
        let classifier = Classifier::new(config).unwrap();
        let req: ChatCompletionRequest = serde_json::from_value(json!({
            "model":"auto",
            "messages":[
                {"role":"user","content":"run the tests"},
                {"role":"assistant","content":"Running tests.","tool_calls":[{"id":"c1","type":"function","function":{"name":"shell","arguments":"{}"}}]},
                {"role":"tool","tool_call_id":"c1","content":"passed"},
                {"role":"user","content":"now fix the failing one"}
            ]
        }))
        .unwrap();
        let result = classifier.classify(&req).await.unwrap();
        assert!(
            result.tier >= ComplexityTier::Hard,
            "tool history should floor to at least Hard, got {:?}",
            result.tier
        );
    }

    #[tokio::test]
    async fn explanatory_context_not_agentic() {
        let config = ClassifierConfig {
            mode: ClassifierMode::Heuristic,
            ..serde_json::from_str("{}").unwrap()
        };
        let classifier = Classifier::new(config).unwrap();
        let r = classifier
            .classify(&request("Explain how to run tests in a Node project"))
            .await
            .unwrap();
        assert!(
            r.tier <= ComplexityTier::Standard,
            "explanatory context should not be agentic, got {:?} ({:?})",
            r.tier,
            r.reasons
        );
        let r = classifier
            .classify(&request("What is a shell script?"))
            .await
            .unwrap();
        assert!(
            r.tier <= ComplexityTier::Standard,
            "explanatory 'what is' should not be agentic, got {:?}",
            r.tier
        );
        let r = classifier
            .classify(&request(
                "Write a unit test for a function that adds two numbers",
            ))
            .await
            .unwrap();
        assert!(
            r.tier <= ComplexityTier::Standard,
            "write a test should be coding not agentic, got {:?}",
            r.tier
        );
        let r = classifier
            .classify(&request("Describe how Docker build works"))
            .await
            .unwrap();
        assert!(
            r.tier <= ComplexityTier::Standard,
            "describe how should not be agentic, got {:?}",
            r.tier
        );
    }

    #[tokio::test]
    async fn light_agentic_routes_to_standard() {
        let config = ClassifierConfig {
            mode: ClassifierMode::Heuristic,
            ..serde_json::from_str("{}").unwrap()
        };
        let classifier = Classifier::new(config).unwrap();
        let r = classifier
            .classify(&request("Check the status of the deployment"))
            .await
            .unwrap();
        assert!(
            r.tier <= ComplexityTier::Standard,
            "light agentic (check status) should not be Hard, got {:?}",
            r.tier
        );
        let r = classifier
            .classify(&request("Show me the logs from the API server"))
            .await
            .unwrap();
        assert!(
            r.tier <= ComplexityTier::Standard,
            "light agentic (show logs) should not be Hard, got {:?}",
            r.tier
        );
    }

    #[tokio::test]
    async fn multi_step_agentic_routes_to_hard() {
        let config = ClassifierConfig {
            mode: ClassifierMode::Heuristic,
            ..serde_json::from_str("{}").unwrap()
        };
        let classifier = Classifier::new(config).unwrap();
        let r = classifier
            .classify(&request("Run the test suite and deploy if all tests pass"))
            .await
            .unwrap();
        assert_eq!(
            r.tier,
            ComplexityTier::Hard,
            "multi-step agentic should be Hard, got {:?}",
            r.tier
        );
        let r = classifier
            .classify(&request(
                "Build the project and push the image to the registry",
            ))
            .await
            .unwrap();
        assert_eq!(
            r.tier,
            ComplexityTier::Hard,
            "multi-step agentic should be Hard, got {:?}",
            r.tier
        );
    }

    // --- Jev evaluation-stage tests -----------------------------------------

    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::Mutex;

    struct MockRequest {
        path: String,
        auth: Option<String>,
        body: serde_json::Value,
    }

    struct MockHttpResponse {
        status: u16,
        body: serde_json::Value,
        /// Delay before responding, to exercise the request deadline.
        delay_ms: u64,
    }

    /// Spawn a one-connection-per-response mock HTTP server. Returns the
    /// base URL and a handle that resolves to the requests the server saw,
    /// in order.
    async fn spawn_mock(
        responses: Vec<MockHttpResponse>,
    ) -> (
        String,
        Arc<Mutex<Vec<MockRequest>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Arc<Mutex<Vec<MockRequest>>> = Arc::default();
        let seen_for_task = seen.clone();
        let handle = tokio::spawn(async move {
            for response in responses {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 8192];
                let mut data = Vec::new();
                let head_end = loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    data.extend_from_slice(&buf[..n]);
                    if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&data[..pos]);
                        let content_length = head
                            .lines()
                            .find_map(|line| {
                                let lower = line.to_lowercase();
                                lower
                                    .starts_with("content-length:")
                                    .then(|| {
                                        lower["content-length:".len()..]
                                            .trim()
                                            .parse::<usize>()
                                            .ok()
                                    })
                                    .flatten()
                            })
                            .unwrap_or(0);
                        if data.len() >= pos + 4 + content_length {
                            break pos;
                        }
                    }
                    if n == 0 {
                        break data
                            .windows(4)
                            .position(|w| w == b"\r\n\r\n")
                            .unwrap_or(data.len());
                    }
                };
                let head = String::from_utf8_lossy(&data[..head_end]).to_string();
                let mut lines = head.lines();
                let request_line = lines.next().unwrap_or_default().to_string();
                let path = request_line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                let auth = lines
                    .filter_map(|line| {
                        let lower = line.to_lowercase();
                        lower
                            .starts_with("authorization:")
                            .then(|| line[15..].trim().to_string())
                    })
                    .next();
                let body_start = (head_end + 4).min(data.len());
                let body = serde_json::from_slice(&data[body_start..])
                    .unwrap_or(serde_json::json!({"mock_parse_error": true}));
                seen_for_task
                    .lock()
                    .await
                    .push(MockRequest { path, auth, body });
                if response.delay_ms > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(response.delay_ms)).await;
                }
                let payload = serde_json::to_vec(&response.body).unwrap();
                let reason = match response.status {
                    200 => "OK",
                    500 => "Internal Server Error",
                    401 => "Unauthorized",
                    _ => "OK",
                };
                let head = format!(
                    "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response.status,
                    reason,
                    payload.len()
                );
                // The client may have already timed out and dropped the
                // connection; delivering the response is best-effort.
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(&payload).await;
                let _ = sock.flush().await;
                let _ = sock.shutdown().await;
            }
        });
        (format!("http://{addr}"), seen, handle)
    }

    fn jev_answer(tier: &str, confidence: f64) -> serde_json::Value {
        serde_json::json!({
            "model": "jev-test",
            "answers": {
                "tier": {"type": "choice", "choice": tier, "confidence": confidence},
                "task": {"type": "choice", "choice": "coding"}
            }
        })
    }

    fn probabilities_answer(tier: &str, p: f64) -> serde_json::Value {
        serde_json::json!({
            "answers": {
                "tier": {"type": "choice", "choice": tier, "probabilities": {tier: p}},
                "task": {"type": "choice", "choice": "reasoning"}
            }
        })
    }

    fn jev_config(base_url: &str, mode: ClassifierMode) -> ClassifierConfig {
        let mut config: ClassifierConfig = serde_json::from_str("{}").unwrap();
        config.mode = mode;
        config.confidence_threshold = 0.65;
        config.jev.enabled = true;
        config.jev.model = "jev-latest".into();
        config.jev.base_url = base_url.into();
        config.jev.api_key = Some("test-key".into());
        config.jev.timeout_ms = 2000;
        config
    }

    #[tokio::test]
    async fn jev_parses_choice_confidence_and_task() {
        let (base_url, seen, server) = spawn_mock(vec![MockHttpResponse {
            status: 200,
            body: jev_answer("standard", 0.84),
            delay_ms: 0,
        }])
        .await;
        let classifier = Classifier::new(jev_config(&base_url, ClassifierMode::Jev)).unwrap();
        let result = classifier
            .classify(&request("Implement a rate-limited middleware"))
            .await
            .unwrap();
        assert_eq!(result.tier, ComplexityTier::Standard);
        assert!(
            (result.confidence - 0.84).abs() < 1e-6,
            "confidence {:?}",
            result.confidence
        );
        assert_eq!(result.classifier, "jev");
        assert_eq!(result.task, Some(TaskType::Coding));
        let requests = seen.lock().await;
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path, "/evaluate");
        assert_eq!(requests[0].auth.as_deref(), Some("Bearer test-key"));
        assert_eq!(requests[0].body["model"], "jev-latest");
        assert_eq!(
            requests[0].body["state"]["request"],
            "Implement a rate-limited middleware"
        );
        assert_eq!(requests[0].body["questions"]["tier"]["type"], "choice");
        assert_eq!(
            requests[0].body["questions"]["tier"]["criteria"]
                .as_object()
                .unwrap()
                .len(),
            5,
            "tier question must offer all five tiers"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn jev_uses_configured_path_and_multimessage_state() {
        let (base_url, seen, server) = spawn_mock(vec![MockHttpResponse {
            status: 200,
            body: jev_answer("simple", 0.7),
            delay_ms: 0,
        }])
        .await;
        let mut config = jev_config(&base_url, ClassifierMode::Jev);
        config.jev.path = Some("/systemone".into());
        let classifier = Classifier::new(config).unwrap();
        let req: ChatCompletionRequest = serde_json::from_value(json!({
            "model": "auto",
            "messages": [
                {"role": "user", "content": "first message"},
                {"role": "user", "content": "second message"}
            ]
        }))
        .unwrap();
        let result = classifier.classify(&req).await.unwrap();
        assert_eq!(result.tier, ComplexityTier::Simple);
        let requests = seen.lock().await;
        assert_eq!(requests[0].path, "/systemone");
        let state = requests[0].body["state"]["request"].as_str().unwrap();
        assert!(state.contains("first message") && state.contains("second message"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn jev_state_includes_tool_context() {
        let (base_url, seen, server) = spawn_mock(vec![MockHttpResponse {
            status: 200,
            body: jev_answer("standard", 0.8),
            delay_ms: 0,
        }])
        .await;
        let classifier = Classifier::new(jev_config(&base_url, ClassifierMode::Jev)).unwrap();
        let req: ChatCompletionRequest = serde_json::from_value(json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "run the test suite"}],
            "tools": [{"type": "function", "function": {"name": "shell", "description": "Run a shell command"}}]
        }))
        .unwrap();
        classifier.classify(&req).await.unwrap();
        let requests = seen.lock().await;
        assert_eq!(
            requests[0].body["state"]["tools"],
            serde_json::json!(["shell"])
        );
        assert_eq!(requests[0].body["state"]["tool_history"], false);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn jev_falls_back_to_probabilities_without_confidence() {
        let (base_url, _seen, server) = spawn_mock(vec![MockHttpResponse {
            status: 200,
            body: probabilities_answer("hard", 0.91),
            delay_ms: 0,
        }])
        .await;
        let classifier = Classifier::new(jev_config(&base_url, ClassifierMode::Jev)).unwrap();
        let result = classifier
            .classify(&request("Design a distributed cache"))
            .await
            .unwrap();
        assert_eq!(result.tier, ComplexityTier::Hard);
        assert!(
            (result.confidence - 0.91).abs() < 1e-6,
            "confidence {:?}",
            result.confidence
        );
        assert_eq!(result.task, Some(TaskType::Reasoning));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn jev_confidence_is_clamped() {
        let (base_url, _seen, server) = spawn_mock(vec![MockHttpResponse {
            status: 200,
            body: jev_answer("trivial", 1.2),
            delay_ms: 0,
        }])
        .await;
        let classifier = Classifier::new(jev_config(&base_url, ClassifierMode::Jev)).unwrap();
        let result = classifier.classify(&request("hello")).await.unwrap();
        assert_eq!(result.tier, ComplexityTier::Trivial);
        assert!(
            result.confidence <= 0.99,
            "confidence {:?}",
            result.confidence
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn jev_invalid_choice_falls_back_to_heuristic() {
        let (base_url, _seen, server) = spawn_mock(vec![MockHttpResponse {
            status: 200,
            body: jev_answer("mega-tier", 0.9),
            delay_ms: 0,
        }])
        .await;
        let classifier = Classifier::new(jev_config(&base_url, ClassifierMode::Jev)).unwrap();
        let result = classifier.classify(&request("Explain DNS")).await.unwrap();
        assert_eq!(
            result.classifier, "heuristic",
            "invalid choice must fall back to heuristic"
        );
        assert_eq!(result.tier, ComplexityTier::Simple);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn jev_http_error_falls_back_to_heuristic() {
        let (base_url, _seen, server) = spawn_mock(vec![MockHttpResponse {
            status: 500,
            body: serde_json::json!({"error": "boom"}),
            delay_ms: 0,
        }])
        .await;
        let classifier = Classifier::new(jev_config(&base_url, ClassifierMode::Jev)).unwrap();
        let result = classifier.classify(&request("Explain DNS")).await.unwrap();
        assert_eq!(result.classifier, "heuristic");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn jev_timeout_falls_back_to_heuristic() {
        let (base_url, _seen, server) = spawn_mock(vec![MockHttpResponse {
            status: 200,
            body: jev_answer("hard", 0.9),
            delay_ms: 2000,
        }])
        .await;
        let mut config = jev_config(&base_url, ClassifierMode::Jev);
        config.jev.timeout_ms = 200;
        let classifier = Classifier::new(config).unwrap();
        let result = classifier
            .classify(&request("Architect a distributed cache"))
            .await
            .unwrap();
        assert_eq!(
            result.classifier, "heuristic",
            "deadline must bound the jev call"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn jev_disabled_endpoint_falls_back_without_calling() {
        let mut config = jev_config("http://127.0.0.1:9", ClassifierMode::Jev);
        config.jev.enabled = false;
        let classifier = Classifier::new(config).unwrap();
        let result = classifier.classify(&request("Explain DNS")).await.unwrap();
        assert_eq!(result.classifier, "heuristic");
    }

    #[tokio::test]
    async fn jev_missing_api_key_falls_back_without_calling() {
        let mut config = jev_config("http://127.0.0.1:9", ClassifierMode::Jev);
        config.jev.api_key = None;
        let classifier = Classifier::new(config).unwrap();
        let result = classifier.classify(&request("Explain DNS")).await.unwrap();
        assert_eq!(result.classifier, "heuristic");
    }

    #[tokio::test]
    async fn override_precedes_jev() {
        let classifier =
            Classifier::new(jev_config("http://127.0.0.1:9", ClassifierMode::Jev)).unwrap();
        let result = classifier
            .classify(&request("@route:reasoning\nhello"))
            .await
            .unwrap();
        assert_eq!(result.classifier, "override");
        assert_eq!(result.tier, ComplexityTier::Reasoning);
    }

    #[tokio::test]
    async fn jev_low_confidence_result_still_returned_in_jev_mode() {
        let (base_url, _seen, server) = spawn_mock(vec![MockHttpResponse {
            status: 200,
            body: jev_answer("standard", 0.3),
            delay_ms: 0,
        }])
        .await;
        let classifier = Classifier::new(jev_config(&base_url, ClassifierMode::Jev)).unwrap();
        let result = classifier
            .classify(&request("Implement an API endpoint"))
            .await
            .unwrap();
        assert_eq!(
            result.classifier, "jev",
            "jev mode must not silently re-classify via heuristic"
        );
        assert_eq!(result.tier, ComplexityTier::Standard);
        server.await.unwrap();
    }

    /// Word matching, not substring matching, for the keyword sets.
    #[test]
    fn keyword_matching_is_word_anchored() {
        assert!(has_word("implement a cache", "implement"));
        assert!(has_word("an api client", "api"));
        assert!(has_word("multi-step intent", "step"), "'-' is a boundary");
        assert!(has_word("code.", "code"), "punctuation is a boundary");
        assert!(!has_word("the capital of france", "api"));
        assert!(!has_word("unicode normalization", "code"));
        assert!(!has_word("interest rate", "rest"));
        assert!(!has_word("decode the payload", "code"));
    }

    #[tokio::test]
    async fn prose_is_not_typed_as_a_coding_task() {
        let classifier = Classifier::new(classifier_config("heuristic")).unwrap();
        for prompt in [
            "What is the capital of France",
            "Explain unicode normalization",
            "Recommend a restaurant in Berlin",
        ] {
            let request: ChatCompletionRequest = serde_json::from_value(json!({
                "model": "auto",
                "messages": [{"role": "user", "content": prompt}]
            }))
            .unwrap();
            let result = classifier.classify(&request).await.unwrap();
            assert!(
                !result.reasons.iter().any(|r| r == "coding-task"),
                "{prompt:?} was typed as a coding task: {:?}",
                result.reasons
            );
        }
    }

    #[tokio::test]
    async fn read_only_tool_names_are_not_agentic() {
        let with_tool = |name: &str| -> ChatCompletionRequest {
            serde_json::from_value(json!({
                "model": "auto",
                "messages": [{"role": "user", "content": "hello"}],
                "tools": [{"type": "function", "function": {"name": name}}]
            }))
            .unwrap()
        };
        let classifier = Classifier::new(classifier_config("heuristic")).unwrap();

        let result = classifier
            .classify(&with_tool("get_user_profile"))
            .await
            .unwrap();
        assert_ne!(
            result.tier,
            ComplexityTier::Hard,
            "a read-only profile lookup must not be agentic"
        );
        assert!(
            !result.reasons.iter().any(|r| r == "agentic-tools"),
            "reasons were {:?}",
            result.reasons
        );

        // A genuine shell tool still is.
        let shell = classifier.classify(&with_tool("run_shell")).await.unwrap();
        assert!(shell.reasons.iter().any(|r| r == "agentic-tools"));
    }

    /// A formal-correctness request must reach the Reasoning tier.
    ///
    /// The prompt has to trip exactly one Reasoning pattern or the guard is
    /// never exercised: "Prove correctness of the CRDT implementation" matches
    /// both `prove` and `correctness` in one regex, so Reasoning scores 14 and
    /// wins at 14 > 10 with or without the guard -- it passes for the wrong
    /// reason. `audit the correctness of the retry logic` matches
    /// `correctness` alone (7) while `retry` still makes it a Coding task (+10).
    #[tokio::test]
    async fn a_coding_task_does_not_outrank_a_matched_reasoning_pattern() {
        let classifier = Classifier::new(classifier_config("heuristic")).unwrap();
        for prompt in [
            "audit the correctness of the retry logic",
            "Prove correctness of the CRDT implementation",
        ] {
            let request: ChatCompletionRequest = serde_json::from_value(json!({
                "model": "auto",
                "messages": [{"role": "user", "content": prompt}]
            }))
            .unwrap();
            let result = classifier.classify(&request).await.unwrap();
            assert_eq!(
                result.tier,
                ComplexityTier::Reasoning,
                "{prompt:?} is a correctness question, got {:?} ({:?})",
                result.tier,
                result.reasons
            );
            assert!(
                !result.reasons.iter().any(|r| r == "coding-task"),
                "{prompt:?} must not be billed as a coding task: {:?}",
                result.reasons
            );
        }
    }

    /// A one-line lookup must not be promoted by a "coding task".
    ///
    /// Only prompts the Trivial *tier table* lists may assert a Trivial tier.
    /// `has_light_agentic` is a task-type signal, not a tier signal: it also
    /// matches prose like "print the version", which the table does not list, so
    /// that one is correctly a Simple lookup. What must hold for all of them is
    /// that none is treated as a coding task.
    #[tokio::test]
    async fn light_agentic_lookups_stay_trivial() {
        let classifier = Classifier::new(classifier_config("heuristic")).unwrap();
        for prompt in ["git status", "git diff"] {
            let request: ChatCompletionRequest = serde_json::from_value(json!({
                "model": "auto",
                "messages": [{"role": "user", "content": prompt}]
            }))
            .unwrap();
            let result = classifier.classify(&request).await.unwrap();
            assert_eq!(
                result.tier,
                ComplexityTier::Trivial,
                "{prompt:?} is a one-line lookup, got {:?} ({:?})",
                result.tier,
                result.reasons
            );
        }

        // A lookup the Trivial table does *not* list is an operational query
        // against a live target, and belongs on the mid tier. It must not reach
        // that tier by being mistaken for software engineering, though: the
        // reason has to be `operational-lookup`, not `coding-task`.
        for prompt in ["Print the version string", "show the config"] {
            let request: ChatCompletionRequest = serde_json::from_value(json!({
                "model": "auto",
                "messages": [{"role": "user", "content": prompt}]
            }))
            .unwrap();
            let result = classifier.classify(&request).await.unwrap();
            assert!(
                !result.reasons.iter().any(|r| r == "coding-task"),
                "{prompt:?} was billed as a coding task: {:?}",
                result.reasons
            );
            assert!(
                result.reasons.iter().any(|r| r == "operational-lookup"),
                "{prompt:?} should be recorded as an operational lookup: {:?}",
                result.reasons
            );
            assert_eq!(
                result.tier,
                ComplexityTier::Standard,
                "{prompt:?} is a lookup against live state, got {:?} ({:?})",
                result.tier,
                result.reasons
            );
        }
    }

    /// `@route:` must work whenever the user turn carries it.
    ///
    /// It used to be read from the first line of the whole concatenated
    /// transcript, so it silently stopped applying for any request opening with
    /// a system prompt or a null-content assistant turn -- which is every
    /// tool-calling turn. A silently ignored explicit instruction is worse than
    /// a rejected one, because the caller cannot tell it did not take effect.
    #[tokio::test]
    async fn route_override_is_found_behind_a_system_prompt() {
        let classifier = Classifier::new(classifier_config("heuristic")).unwrap();

        for messages in [
            json!([
                {"role": "system", "content": "You are a helpful assistant."},
                {"role": "user", "content": "@route:hard\ndesign a system"}
            ]),
            json!([
                {"role": "assistant", "content": null},
                {"role": "user", "content": "@route:hard\ndesign a system"}
            ]),
            json!([{"role": "user", "content": "@route:hard\ndesign a system"}]),
        ] {
            let request: ChatCompletionRequest =
                serde_json::from_value(json!({"model": "auto", "messages": messages})).unwrap();
            let result = classifier.classify(&request).await.unwrap();
            assert_eq!(
                result.tier,
                ComplexityTier::Hard,
                "override was dropped for {:?}",
                request.messages
            );
            assert_eq!(result.classifier, "override");
        }

        // Case-insensitive, like every other pattern in the file.
        let shouted: ChatCompletionRequest = serde_json::from_value(json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "@ROUTE:Hard\ndesign a system"}]
        }))
        .unwrap();
        assert_eq!(
            classifier.classify(&shouted).await.unwrap().tier,
            ComplexityTier::Hard
        );

        // An unknown tier must not be honoured.
        let bogus: ChatCompletionRequest = serde_json::from_value(json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "@route:banana\nhello"}]
        }))
        .unwrap();
        assert_ne!(
            classifier.classify(&bogus).await.unwrap().classifier,
            "override"
        );
    }

    /// A Jev answer with no confidence signal must not be reported as
    /// confident.
    ///
    /// It fell back to `default_confidence()` = 0.70, which is above the 0.65
    /// threshold in the shipped config, and `ClassifierMode::Jev` applies no
    /// threshold of its own -- so a bare `{"choice":"hard"}` came back as a
    /// confident Hard and the heuristic fallback never got a look in. A Jev
    /// outage presenting as malformed JSON was therefore indistinguishable from
    /// Jev working correctly, which is the opposite of the documented
    /// "availability over accuracy" behaviour.
    #[tokio::test]
    async fn jev_without_a_confidence_signal_falls_back() {
        for body in [
            r#"{"answers":{"tier":{"choice":"hard"}}}"#,
            r#"{"answers":{"tier":{"choice":"hard","confidence":null}}}"#,
            r#"{"answers":{"tier":{"choice":"hard","confidence":"0.95"}}}"#,
            r#"{"answers":{"tier":{"choice":"hard","probabilities":{"standard":0.9}}}}"#,
        ] {
            let base = spawn_answer(serde_json::from_str(body).unwrap()).await.0;
            let classifier = Classifier::new(jev_config(&base, ClassifierMode::Jev)).unwrap();
            let request: ChatCompletionRequest = serde_json::from_value(json!({
                "model": "auto",
                "messages": [{"role": "user", "content": "design a resilient multi-region migration"}]
            }))
            .unwrap();
            let result = classifier.classify(&request).await.unwrap();
            assert_ne!(
                result.classifier, "jev",
                "{body} must not be accepted as a confident Jev answer"
            );
        }
    }

    /// A fenced JSON answer must be usable, not silently discarded.
    #[tokio::test]
    async fn a_fenced_json_answer_is_not_discarded() {
        let base = spawn_answer(serde_json::json!({
            "choices": [{"message": {"content":
                "```json\n{\"tier\":\"hard\",\"confidence\":0.9}\n```"
            }}]
        }))
        .await
        .0;
        let classifier = Classifier::new(llm_config(&base)).unwrap();
        let request: ChatCompletionRequest = serde_json::from_value(json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "design a resilient multi-region migration"}]
        }))
        .unwrap();
        let result = classifier.classify(&request).await.unwrap();
        assert_eq!(result.classifier, "local_llm");
        assert_eq!(result.tier, ComplexityTier::Hard);
    }
}
