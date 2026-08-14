use miser_types::{
    ChatCompletionRequest, ClassificationResult, ClassifierConfig, ClassifierMode, ComplexityTier,
    MessageContent, TaskType,
};
use regex::RegexSet;
use serde::Deserialize;
use serde_json::json;
use std::time::Instant;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClassifierError {
    #[error("classifier request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("classifier returned invalid response: {0}")]
    Parse(#[from] serde_json::Error),
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
                r"(?i)^\s*(hello|hi|hey|thanks|thank you|ok|okay)\s*[!.]*\s*$",
                r"(?i)\b(git status|git diff|git log)\b",
                r"(?i)^(what is|what's)\s+(your\s+name|2\s*\+\s*2|the\s+time|the\s+date|my\s+name)\b",
                r"(?i)\b(rename|uppercase|lowercase|trim)\b.*\b(variable|file|string|line)\b",
                r"(?i)^\s*(yes|no|true|false)\s*[.!]?\s*$",
            ])?,
            simple: RegexSet::new([
                r"(?i)\b(explain|summarize|compare|convert|translate|format|describe|tell\s+me)\b",
                r"(?i)\b(write|create)\s+(a|an)\s+(small|simple)?\s*\w*\s*(function|class|regex|script|interface)\b",
                r"(?i)\b(add|change|fix)\s+(a|the)\s+(comment|null check|format)\b",
                r"(?i)\b(dockerfile|docker-compose|readme|migration|docker)\b",
                r"(?i)\b(sql|query|select|insert|index)\b.*\b(write|create|add|optimize)\b",
                r"(?i)\b(unit test|snapshot test|test for)\b",
                r"(?i)\b(cors|semicolon|trailing|whitespace|quotes|tab|spaces)\b",
                r"(?i)\b(git command|curl command|shell command)\b",
                r"(?i)\b(type|interface|schema)\b.*\b(for|with)\b.*\b(id|name|email|field)\b",
                r"(?i)\b(dependency|package|install|import)\b.*\b(add|fix|update)\b",
                r"(?i)^(what is|what's|what are)\s+",
                r"(?i)\b(npm|yarn|pip|cargo)\b.*\b(what|how|explain|difference)\b",
                r"(?i)\b(ci.cd|pipeline|workflow)\b.*\b(what|how|explain|about|tell)\b",
            ])?,
            standard: RegexSet::new([
                r"(?i)\b(implement|build|integrate|debug|refactor|test|endpoint|migration)\b",
                r"(?i)\b(api|database|authentication|middleware|component)\b.*\b(add|create|implement|design)\b",
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
                r"(?i)\b(architect|distributed|production incident|threat-model|zero-downtime|multi-region)\b",
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

    pub async fn classify(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<ClassificationResult, ClassifierError> {
        let started = Instant::now();
        let text = request_text(request);
        if let Some((tier, reason)) = override_tier(&text) {
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
        for (set, index, weight) in sets {
            let matches = set.matches(text).into_iter().count() as i32;
            scores[index].1 += matches * weight;
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
        if classification_task == Some(TaskType::Coding) && scores[1].1 <= 1 {
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
            return Err(ClassifierError::Parse(
                serde_json::from_str::<serde_json::Value>("null").unwrap_err(),
            ));
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
        let parsed: LlmResult = serde_json::from_str(content)
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

fn override_tier(text: &str) -> Option<(ComplexityTier, String)> {
    let first = text.lines().next()?.trim();
    let tier = first.strip_prefix("@route:")?;
    let parsed = match tier {
        "trivial" => ComplexityTier::Trivial,
        "simple" => ComplexityTier::Simple,
        "standard" => ComplexityTier::Standard,
        "hard" => ComplexityTier::Hard,
        "reasoning" => ComplexityTier::Reasoning,
        _ => return None,
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
        return Some(TaskType::Coding);
    }
    coding_or_reasoning(&lower)
}

fn coding_or_reasoning(lower: &str) -> Option<TaskType> {
    if lower.contains("code")
        || lower.contains("implement")
        || lower.contains("function")
        || lower.contains("python")
        || lower.contains("typescript")
        || lower.contains("debug")
        || lower.contains("api")
        || lower.contains("endpoint")
        || lower.contains("retry")
        || lower.contains("bug")
        || lower.contains("rest")
    {
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
    request.tools.as_ref().is_some_and(|tools| {
        tools.iter().any(|tool| {
            let tool_str = tool.to_string().to_lowercase();
            tool_str.contains("shell")
                || tool_str.contains("bash")
                || tool_str.contains("execute")
                || tool_str.contains("run")
                || tool_str.contains("file")
                || tool_str.contains("search")
                || tool_str.contains("grep")
                || tool_str.contains("command")
                || tool_str.contains("terminal")
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
}
