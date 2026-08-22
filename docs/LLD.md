# Low-Level Design

## 1. Workspace

```text
crates/
  miser-types/         serde request/config/result types
  miser-classifier/    override, structural, heuristic, LLM stages
  miser-policy/        tier-to-route decision engine + quality scoring
  miser-provider/      reqwest OpenRouter adapter
  miser-gateway/       axum binary, HTTP handlers, auth, cache
  miser-evals/         JSONL offline evaluator + quality harness
config/                sanitized TOML configuration
deploy/                hardened systemd unit
evals/                 versioned labeled cases (25 + 50 + 100)
scripts/               VPS benchmark runners
prototypes/typescript/ original Bun/TypeScript prototype
.github/workflows/     CI/CD pipeline
docs/                  HLD, LLD, security, operations, evaluation
```

## 2. Data contracts

### Incoming request

`ChatCompletionRequest` models common OpenAI fields and stores unsupported fields in `extra: BTreeMap<String, Value>`. This prevents the gateway from dropping provider-specific options such as reasoning settings, parallel tool calls, metadata, or cache controls.

Required fields: `model: String`, `messages: Vec<ChatMessage>`.

Optional fields: stream, temperature, top_p, max_tokens, max_completion_tokens, tools, tool_choice, response_format, stop, seed, user.

### Classification result

```text
ClassificationResult {
  tier: ComplexityTier,       // Trivial < Simple < Standard < Hard < Reasoning
  confidence: f32,
  reasons: Vec<String>,
  classifier: String,          // "override" | "heuristic" | "local_llm" | "cloud_llm"
  latency_ms: u64,
  task: Option<TaskType>,      // Chat | Coding | Reasoning | ...
  risk: Option<RiskLevel>,
  privacy: Option<PrivacyLevel>,
}
```

### API key

```text
ApiKey {
  id: String,                  // key_<12 random chars>
  key_hash: String,            // SHA-256 hash (SHA-256, 64 hex chars)
  owner: String,
  created_at: u64,             // Unix timestamp
  active: bool,
  allowed_tiers: Vec<String>,  // future: per-key tier restrictions
  rate_limit_rpm: Option<u32>, // future: per-key rate limits
  monthly_budget_usd: Option<f64>, // future: per-key budget
}
```

Stored in `/var/lib/miser/keys.json` as `{"keys": [ApiKey, ...]}`.

### Route

```text
TierModelRouteConfig {
  model: String,
  max_tokens: Option<u32>,
  temperature: Option<f32>,
  max_cost_per_1m: Option<CostLimit>,
  provider: Option<String>,
}
```

### Quality config

```text
QualityConfig {
  enabled: bool,
  minimum_score: f32,
  escalate_on_failure: bool,
  judge: Option<ClassifierEndpointConfig>,
}
```

## 3. Classifier internals

### Override stage

Protocol: `@route:<tier>` at the beginning of the first user text line. Valid values: `trivial`, `simple`, `standard`, `hard`, `reasoning`. Invalid or embedded values treated as ordinary content. Confidence: 1.0.

### Text extraction

Text content collected from all messages. Text content parts retained; images/audio/refusals are not converted. Tool presence and response format supplied as structural signals.

### Task detection

Keyword-based task classification:
- **Coding**: code, implement, function, python, typescript, debug, api, endpoint, retry, bug, rest
- **Reasoning**: prove, derive, algorithm, amortized, invariant, converge, halting, reduction, bayesian
- **Chat**: everything else

Coding tasks receive +10 score boost in heuristic, forcing them to at least `standard` tier.

### Heuristic stage

Regexes compiled once into `RegexSet` during classifier construction. Five tier pattern sets with weighted matches:

| Tier | Weight | Pattern count | Example patterns |
|---|---:|---:|---|
| trivial | 5 | 5 | greetings, git status, rename, yes/no |
| simple | 3 | 10 | explain, write function, dockerfile, sql query, unit test |
| standard | 4 | 10 | implement, rate limit, jwt, oauth, kubernetes, react optimize |
| hard | 6 | 10 | architect, distributed, service mesh, event sourcing, chaos |
| reasoning | 7 | 6 | prove, derive, halting, reduction, bayesian, amortized |

Structural additions:
- Tools present: +4 to standard
- Messages > 10: +3 to standard
- Response format present: +2 to standard
- Coding task: +10 to standard

Maximum score wins. Confidence: `0.55 + score/30.0` capped at 0.95.

### LLM stage

OpenAI-compatible `/chat/completions` with:
- `temperature: 0`
- `max_tokens: 180`
- `think: false`
- `response_format: { type: json_object }`

Each call has a configured timeout (local: 800ms, cloud: 1500ms). Result parsed into constrained tier enum. Invalid responses become classifier failures triggering fallback.

### Hybrid decision (concurrent)

1. Always run override and heuristic stages.
2. If heuristic confidence ≥ threshold (0.65), return immediately.
3. If local LLM enabled, pin future as `Box::pin`.
4. If cloud LLM enabled, pin future as `Box::pin`.
5. `tokio::select!` on both futures:
   - First to complete with confidence ≥ threshold wins.
   - If first completes below threshold, await the other.
   - If first errors, await the other.
   - If both error, return heuristic.
6. This eliminates serial 13s local-then-cloud wait on ambiguous prompts.

## 4. Policy engine

### Tier selection

```rust
pub fn effective_tier(&self, request, classification) -> ComplexityTier {
    let mut tier = classification.tier;
    if classification.confidence < threshold {
        tier = max(tier, Standard);
    }
    if request.tools present { tier = max(tier, Standard); }
    if request.response_format present { tier = max(tier, Standard); }
    if classification.task == Reasoning { tier = max(tier, Reasoning); }
    tier
}
```

### Quality scoring

`deterministic_quality()` checks:
- Empty content → score 0.0
- Structured output with invalid JSON → score × 0.25
- Coding task without code blocks and < 80 chars → score 0.3
- Otherwise → score 0.85 (if ≥ 40 chars) or 0.65

`parse_judge()` parses LLM judge JSON response with score and passed fields.

## 5. Gateway handlers

### Authentication flow

```
extract bearer token from Authorization header
if admin_key empty AND key store empty:
    open access (initial setup mode)
else:
    validate bearer against key store (constant-time SHA-256 compare)
    on failure: check if bearer matches admin_key
    if neither: return 401
```

### `POST /v1/chat/completions`

1. Authenticate.
2. Generate UUID request ID.
3. Serialize request to JSON, compute FNV cache hash.
4. Check exact cache → return cached bytes with `x-miser-cache: hit-exact`.
5. Classify (override → structural → heuristic → LLM if needed).
6. Select route via policy engine (with capability floors).
7. Set `request.model` to route model.
8. Respect client `max_tokens`; fill tier default only if absent.
9. Forward to OpenRouter.
10. If non-streaming and success:
    - Buffer response bytes.
    - Store in cache.
    - Return with routing headers and `x-miser-cache: miss`.
11. If streaming: forward upstream bytes with routing headers.

### Admin endpoints

- `POST /admin/keys` — generate `miser_<43chars>`, hash, persist, return raw key once.
- `GET /admin/keys` — list all keys with hashes redacted.
- `GET /admin/keys/{id}` — single key details.
- `DELETE /admin/keys/{id}` — remove key from store.

All admin endpoints require `Authorization: Bearer <admin_key>`.

## 6. Cache

### Exact cache (`cache.rs`)

```rust
pub struct ResponseCache {
    entries: Mutex<HashMap<u64, CacheEntry>>,
    max_entries: usize,  // 10,000
    ttl: Duration,        // 5 minutes
}
```

- Key: FNV-1a hash of normalized request body (excludes `model`, `user`, `seed`).
- Value: response bytes, status code, safe headers, insertion time.
- LRU eviction when full.
- TTL expiry on lookup.

### Semantic cache (`semantic_cache.rs`)

Implemented but disabled (`max_entries: 0`). TF-IDF bag-of-words embedding with cosine similarity. Disabled because coding prompts share vocabulary ("function", "test", "implement") causing false-positive cache hits at any threshold below 1.0. Future: use proper sentence embeddings (MiniLM) or exact-match only.

## 7. Provider adapter

`Provider` owns one reusable `reqwest::Client` with:
- Rustls TLS (no OpenSSL dependency)
- Connection pooling
- No redirects (SSRF prevention)
- No default timeout (streaming responses)

`rewrite_body()` replaces `model` and merges `provider` preferences. `safe_response_headers()` filters to an allowlist of 5 headers, excluding hop-by-hop headers (`content-length`, `transfer-encoding`, `set-cookie`).

## 8. Configuration

`config/miser.toml`:
- Server: host, port, api_key (empty for key-based auth), admin_key
- Classifier: mode, stages, confidence_threshold, local_llm, cloud_llm
- Quality: enabled, minimum_score, escalate_on_failure
- Provider: base_url, api_key, api_key_env, provider_preferences
- Tiers: 5 tiers with model and max_tokens

Secrets resolved from environment (`OPENROUTER_API_KEY`, `MISER_ADMIN_KEY`, `MISER_KEYS_FILE`).

## 9. Error model

- Invalid API key: `401` JSON error
- Inactive key: `403` JSON error
- Classifier/provider failure: `502` JSON error with message
- Upstream status preserved for successful responses
- No internal stack traces in error responses
- Request ID in all error responses

## 10. Testing strategy

- Unit tests: serde preservation, tier ordering, heuristic representatives, overrides, provider rewriting, auth omission, response-header filtering, policy capability floors.
- Offline evals: 25-case, 50-case, and 100-case labeled corpora covering trivial/simple/standard/hard/reasoning with coding, devops, database, security, algorithm, and architecture categories.
- Live evals: GLM 5.2 judge scores completion quality across Miser, OpenRouter Auto, and fixed model baselines.
- CI gates: `cargo fmt --check`, `cargo check`, `cargo test`, `cargo clippy -D warnings`, `cargo audit`, `cargo-deny`, Gitleaks, Trivy.
- VPS deployment: native Rust build, atomic binary swap, systemd restart, health check.

## 11. Tier-to-model mapping

| Tier | Model | Type | Cost | Max tokens |
|---|---|---|---|---:|
| trivial | qwen/qwen3.7-flash | Open-weight | Free | 512 |
| simple | deepseek/deepseek-v4-flash | Open-weight | Free | 1024 |
| standard | qwen/qwen3-coder-flash | Open-weight | Free | 2048 |
| hard | anthropic/claude-sonnet-4 | Frontier | Paid | 4096 |
| reasoning | z-ai/glm-5.2 | Frontier | Free | 4096 |

Open-weight models handle 80%+ of software engineering tasks at zero cost. Frontier models reserved for architecture, security, and formal reasoning.

## 12. Benchmark results

### Classification accuracy (25-case corpus)

| Strategy | Exact accuracy | Adjacent accuracy | p50 latency |
|---|---:|---:|---:|
| Miser heuristics | 92% | 92% | <1ms |
| OpenRouter Auto | 52% | 84% | 4.16s |

### Completion quality (GLM 5.2 judge, 100 real-world SE cases)

| Strategy | Quality | Pass rate | Classification | p95 | Tokens/quality |
|---|---:|---:|---:|---:|---:|
| Miser Auto | 0.5928 | 62% | **54%** | 30.0s | 1,503 |
| OpenRouter Auto | 0.5683 | 52% | 0% | 19.0s | 713 |
| GPT-4.1-mini | 0.7901 | 82% | 0% | 15.9s | 508 |
| Claude Sonnet 4 | 0.7868 | 82% | 0% | 14.5s | 616 |

Miser is the only gateway with per-request classification routing. Miser beats OpenRouter Auto on quality and pass rate. Fixed frontier models (GPT-4.1-mini, Claude Sonnet 4) lead quality as expected since they use the strongest model for every request. Miser's advantage is cost: 80%+ of requests route to free open-weight models.

### Per-tier classification accuracy

| Tier | Accuracy |
|---|---:|
| trivial | 10% |
| simple | 10% |
| standard | 90% |
| hard | 60% |
| reasoning | 100% |

Known issue: trivial and simple coding tasks are over-classified to standard due to the +10 coding boost. Improvement: apply coding boost only when no trivial/simple pattern matches, or reduce boost weight.
