# Low-Level Design

## 1. Workspace

```text
crates/
  miser-types/       serde request/config/result types
  miser-classifier/  override, structural, heuristic, LLM stages
  miser-policy/      tier-to-route decision engine
  miser-provider/    reqwest OpenRouter adapter
  miser-gateway/     axum binary and HTTP handlers
  miser-evals/       JSONL offline evaluator
config/              sanitized TOML configuration
 deploy/             hardened systemd unit
 evals/              versioned labeled cases
```

## 2. Data contracts

### Incoming request

`ChatCompletionRequest` models common OpenAI fields and stores unsupported fields in `extra: BTreeMap<String, Value>`. This prevents the gateway from dropping provider-specific options such as reasoning settings, parallel tool calls, metadata, or cache controls.

Required fields:

- `model: String`
- `messages: Vec<ChatMessage>`

Optional fields include stream, temperature, token limits, tools, tool choice, response format, stop, seed, and user.

### Classification result

```text
ClassificationResult {
  tier: ComplexityTier,
  confidence: f32,
  reasons: Vec<String>,
  classifier: String,
  latency_ms: u64,
  task: Option<TaskType>,
  risk: Option<RiskLevel>,
  privacy: Option<PrivacyLevel>
}
```

`ComplexityTier` is ordinal: `Trivial < Simple < Standard < Hard < Reasoning`.

### Route

```text
TierModelRouteConfig {
  model: String,
  max_tokens: Option<u32>,
  temperature: Option<f32>,
  max_cost_per_1m: Option<CostLimit>,
  provider: Option<String>
}
```

## 3. Classifier internals

### Override stage

The current protocol is `@route:<tier>` at the beginning of the first user text line. Valid values are the five lowercase tier names. Invalid or embedded values are treated as ordinary content. A valid override has confidence 1.0 and bypasses semantic classification.

### Text extraction

Text content is collected from all messages. Text content parts are retained; images/audio/refusals are not converted into prompt text. Tool presence and response format are supplied as structural signals.

### Heuristic stage

Regexes are compiled once into `RegexSet` instances during classifier construction. Each tier has weighted pattern matches. Structural additions increase standard complexity for tools, deep conversations, and structured output. The maximum score wins; no external request is made.

The current heuristic implementation is intentionally interpretable. Reasons are compact internal labels and must not be exposed if they contain user-derived text.

### LLM stage

The local and cloud stages use the OpenAI-compatible `/chat/completions` contract:

```json
{
  "model": "classifier-model",
  "messages": [
    {"role":"system","content":"Return only a tier JSON object"},
    {"role":"user","content":"serialized request text"}
  ],
  "temperature": 0,
  "max_tokens": 180,
  "think": false,
  "response_format": {"type":"json_object"}
}
```

Each call has a configured request timeout and optional bearer key. The result is parsed into a constrained tier enum. Invalid responses become classifier failures and trigger the configured fallback.

### Hybrid decision

1. Always run override and heuristic stages.
2. If heuristic confidence meets threshold, return it.
3. If enabled, call local LLM with its timeout.
4. If local fails and cloud is enabled, call cloud LLM.
5. If all optional stages fail, return heuristic output.

A production hardening item is a semaphore/circuit breaker around each LLM endpoint to prevent queue amplification under load.

## 4. Gateway handlers

### `GET /health/live`

Returns a small static JSON response. It must not call OpenRouter.

### `GET /health/ready`

Returns configured route count. Future readiness checks may include cached provider health.

### `GET /v1/models`

Forwards model discovery to OpenRouter and returns an empty data response if discovery fails. This endpoint should be protected in public deployments.

### `POST /v1/chat/completions`

Handler sequence:

1. Read `Authorization`.
2. Deserialize JSON into `ChatCompletionRequest`.
3. Generate UUID request ID.
4. Classify.
5. Select configured route.
6. Set route model and optional output controls.
7. Serialize request back to JSON.
8. Forward with provider adapter.
9. Copy safe response headers.
10. Add `x-miser-*` routing headers.
11. Stream response bytes with `Body::from_stream`.

The current handler supports transparent streaming response bodies. Future work should add a request body limit and avoid exposing raw internal error text.

## 5. Provider adapter

`Provider` owns one reusable `reqwest::Client`. It trims trailing base URL slashes, adds bearer authentication only when configured, merges provider preferences into the request body, and forwards to `/chat/completions`.

Safe response headers are allowlisted. Hop-by-hop headers, cookies, upstream authorization, and arbitrary headers are not copied.

Provider preferences are merged under the OpenRouter `provider` request field. Configured policy should win over client-provided preferences for restricted deployments.

## 6. Configuration

The current gateway loads TOML directly into `GatewayConfig`. Secrets are read from an environment variable named by `provider.extra.api_key_env`, normally `OPENROUTER_API_KEY`. The checked-in TOML contains an empty API key and no secret.

Production configuration should be:

- owned by root or the service administrator;
- readable only by the gateway user;
- injected through a secret manager or mode-600 environment file;
- excluded from source archives and logs.

## 7. Error model

- Invalid authentication: `401`.
- Classifier/provider/configuration failures in the current MVP: `502` JSON error.
- Upstream status and body are preserved for successful provider responses.
- Future errors should use stable OpenAI-compatible error fields with request ID and no internal stack traces.

## 8. Concurrency and resource limits

The async runtime is Tokio. Reqwest connection pooling is shared per provider. The systemd unit sets `LimitNOFILE=65536`. Production hardening should add:

- Axum body-size limit.
- Per-key rate limiter.
- Per-key concurrency semaphore.
- Classifier endpoint semaphore.
- Maximum tool count and message count.
- Upstream connect/read/idle timeouts.
- Circuit breakers for provider and classifier health.

## 9. Testing strategy

- Unit tests cover serde preservation, tier ordering, heuristic representatives, overrides, provider rewriting, auth omission, and response-header filtering.
- Offline evals cover balanced tier representatives, tools, schemas, overrides, and adversarial lexical cases.
- Provider integration tests should use a local mock server for JSON and SSE.
- Gateway smoke tests should verify health, auth, route headers, unknown-field preservation, upstream errors, and client disconnect cancellation.
- CI runs format, check, tests, clippy, audit, deny, Gitleaks, and Trivy.
