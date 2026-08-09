# High-Level Design

## 1. Purpose

Miser is a remotely hosted, low-latency AI gateway written in Rust. Any OpenAI-compatible harness sends one request to Miser. Miser classifies the workload, applies safety and capability policy, chooses a cost-efficient model, and forwards the request to OpenRouter.

The gateway is stateless. It does not persist prompts or completions. API keys are persisted as SHA-256 hashes in a local file store.

## 2. Goals

- OpenAI-compatible chat-completions ingress with transparent streaming.
- Per-request complexity classification: heuristic, local-LLM, cloud-LLM, and hybrid modes.
- Concurrent first-wins classification using `tokio::select!` to minimize classification latency.
- Cost-aware tier-to-model routing with open-weight models for trivial/simple/standard tasks and frontier models for hard/reasoning tasks.
- Exact-match response caching with FNV hash and 5-minute TTL to eliminate repeated inference.
- API key authentication with admin management endpoints and SHA-256 hashed key store.
- Quality escalation for non-streaming responses (deterministic checks, one-tier retry).
- Conservative fallback when a classifier is unavailable.
- Deployable as a non-root VPS service behind nginx with TLS.
- Reproducible offline and live evaluation with GLM 5.2 as independent quality judge.
- Safe open-source operation without committed secrets, enforced by Gitleaks and Trivy in CI.

## 3. Non-goals

- Training a proprietary foundation model.
- Storing prompt bodies or completions at rest.
- Arbitrary user-selected upstream URLs (SSRF prevention).
- Multi-region consensus or a distributed database.
- Guaranteed quality equivalence between models.
- Semantic similarity caching with embedding models (explored, disabled due to false positives).

## 4. Context and deployment

```text
OpenCode / Codex / Aider / OpenAI SDK
                 |
                 | HTTPS (TLS via nginx + Let's Encrypt)
                 v
         miser.rajeev.me :443
                 |
                 | nginx reverse proxy (100M body limit)
                 v
         Miser Gateway :8787 (Rust/Axum)
                 |
       +-----------+-----------+
       |           |           |
  API key     Classifier    Cache
  validation  (heuristic    (FNV hash
  (SHA-256    + local LLM   exact-match)
  compare)    + cloud LLM       |
              concurrent)       |
       |           |           |
       +-----------+-----------+
                 |
          Tier policy engine
          (capability floors,
           task-aware routing)
                 |
                 v
          OpenRouter API
          (provider.sort=price)
                 |
                 v
          Selected model:
          trivial  → qwen3.7-flash (free)
          simple   → deepseek-v4-flash (free)
          standard → qwen3-coder-flash (free)
          hard     → claude-sonnet-4
          reasoning→ glm-5.2 (free)
```

The gateway is the central integration point. It runs as a systemd service under a non-root `miser` user with `ProtectSystem=strict`, `NoNewPrivileges`, and `PrivateTmp`. nginx terminates TLS with Let's Encrypt certificates and forwards to localhost:8787.

## 5. Main components

### Gateway server (`miser-gateway`)

Axum HTTP server handling:
- `POST /v1/chat/completions` — classified, routed, cached, streamed completion
- `GET /v1/models` — upstream model discovery
- `GET /health/live` — process liveness
- `GET /health/ready` — configuration readiness
- `POST /admin/keys` — create API key (admin auth)
- `GET /admin/keys` — list API keys (admin auth, hashes redacted)
- `GET /admin/keys/{id}` — key details (admin auth)
- `DELETE /admin/keys/{id}` — delete key (admin auth)

### Authentication (`auth.rs`)

- User API keys validated on every `/v1/` request using constant-time SHA-256 hash comparison.
- Admin API key protects `/admin/` endpoints.
- Keys stored in `/var/lib/miser/keys.json` (mode 600, owned by `miser` user).
- Open access mode when no keys and no admin key are configured (initial setup).
- Key format: `miser_<43 random base62 chars>`.

### Shared types (`miser-types`)

OpenAI request envelopes with `#[serde(flatten)]` for unknown fields, content parts, ordered complexity tiers, task/risk/privacy/latency enums, classification results, classifier configuration, provider configuration, quality configuration, cache configuration, and route configuration.

### Classifier (`miser-classifier`)

Five-stage pipeline with four modes:
- **Override**: `@route:<tier>` at byte zero, confidence 1.0
- **Structural**: tools present, JSON schema, message count, system prompt length, response format
- **Heuristic**: compiled `RegexSet` with weighted tier scoring (trivial/simple/standard/hard/reasoning), coding-task detection with +10 boost, zero external calls
- **LLM**: OpenAI-compatible `/chat/completions` with `temperature:0`, `response_format:json_object`, bounded timeout
- **Hybrid**: accept high-confidence heuristics; if below threshold, race local + cloud LLMs concurrently via `tokio::select!`; first result above threshold wins; fallback to heuristic on failure

### Policy (`miser-policy`)

- Tier-to-route mapping with capability floors: tools → standard, structured output → standard, reasoning task → reasoning tier.
- `effective_tier()` escalates when confidence is below threshold.
- `next()` provides one-tier escalation for quality retry.
- Quality scoring: deterministic checks for empty content, invalid JSON, insufficient coding output, and optional LLM judge parsing.

### Provider (`miser-provider`)

- Reusable `reqwest::Client` with Rustls, connection pooling, no redirects.
- OpenRouter forwarding with provider preference injection (`sort=price`, `allow_fallbacks=true`).
- Safe response header allowlist (no hop-by-hop headers).
- Classifier chat support for JSON-constrained responses.

### Cache (`cache.rs`)

- Exact-match FNV hash cache: 10,000 entries, 5-minute TTL, LRU eviction.
- Key: normalized request body (model, user, seed excluded).
- Stores: response bytes, status, headers.
- `x-miser-cache: hit-exact | miss` response header.
- Semantic TF-IDF cache implemented but disabled (false positives on similar coding prompts).

### Evaluator (`miser-evals`)

- Offline classification eval: JSONL corpus, exact/adjacent accuracy, confusion matrix.
- Quality eval: required-content coverage, structured-output validity, optional LLM judge.
- CLI: `--mode heuristic|local_llm|cloud_llm|hybrid`, `--quality <path>`.

## 6. Request lifecycle

1. Client sends `POST /v1/chat/completions` with `Authorization: Bearer miser_<key>`.
2. Gateway validates API key against hashed key store (constant-time comparison).
3. Gateway parses OpenAI envelope preserving unknown fields via `#[serde(flatten)]`.
4. Gateway computes FNV hash of normalized request and checks exact cache.
5. On cache hit: return cached response with `x-miser-cache: hit-exact`.
6. On cache miss: classifier checks override, structural signals, heuristic scoring.
7. In hybrid mode: if heuristic confidence < threshold, race local + cloud LLMs concurrently.
8. Policy engine applies capability floors and selects tier-to-model route.
9. Gateway respects client `max_tokens` if specified; fills tier defaults only when absent.
10. Provider forwards to OpenRouter with price-sorted provider preferences.
11. For non-streaming success: buffer response, store in cache, return with routing headers.
12. For streaming: forward upstream bytes transparently with routing headers.
13. Response headers: `x-miser-request-id`, `x-miser-tier`, `x-miser-model`, `x-miser-classifier`, `x-miser-confidence`, `x-miser-cache`.

## 7. Availability and degradation

- Heuristics remain available if Ollama or cloud classifier is down.
- Concurrent classification: local timeout 800ms, cloud timeout 1500ms.
- Classification failure falls back to heuristic result.
- Provider failure returned to client as 502 JSON error.
- Systemd restarts crashed process with `Restart=always`.
- Readiness confirms configuration and route availability without upstream calls.
- Cache reduces repeat-request latency to <1ms on exact hits.

## 8. Scalability

The gateway is stateless and can run behind a load balancer. Horizontal replicas require shared configuration and independent caches. Future shared rate limits and budgets require an external store.

Latency priorities:
1. Exact cache hits return in <1ms with zero token cost.
2. High-confidence heuristics (<1ms) skip LLM classification entirely.
3. Concurrent local + cloud classification races both, first-wins.
4. Client `max_tokens` respected to avoid over-generation.
5. Tier-based token limits (trivial: 512, simple: 1024, standard: 2048, hard: 4096).
6. `provider.sort=price` selects cheapest upstream provider.
7. Open-weight models (Qwen, DeepSeek) used for 80%+ of tasks at zero cost.

## 9. Observability

Structured tracing logs (JSON format) without prompt bodies or credentials. Response headers expose routing metadata. Planned metrics include request count, classification duration, selected tier/model, provider status, time-to-first-byte, upstream token usage, cache hit rate, and cost per request.

## 10. Security

- TLS terminated by nginx with Let's Encrypt (auto-renewing).
- API key authentication with SHA-256 hashed storage.
- Admin key separate from user keys.
- `ProtectSystem=strict` prevents writes outside `/var/lib/miser`.
- No prompt bodies or API keys in logs.
- Safe response header allowlist (no cookies, no hop-by-hop headers).
- Upstream URL is configuration-only (no client-controlled SSRF).
- CI scans: Gitleaks, Trivy, cargo-audit, cargo-deny.
- `.gitleaks.toml` allowlist for known false positives.
- Key rotation: revoke and create via admin API, no history rewrite needed.

## 11. CI/CD

GitHub Actions workflow (`.github/workflows/deploy.yml`):
1. **Verify**: `cargo fmt --check`, `cargo check`, `cargo test`, `cargo clippy -D warnings`, secret scan.
2. **Security**: `cargo audit`, `cargo-deny` (advisories, bans, licenses, sources), Gitleaks, Trivy.
3. **Deploy**: SSH to VPS, upload source, `cargo build --release`, atomic binary swap, systemd restart, health check.

GitHub Secrets: `VPS_HOST`, `VPS_USER`, `VPS_SSH_KEY`, `OPENROUTER_API_KEY`.

## 12. Evaluation methodology

- Classification corpus: `evals/cases.jsonl` (25 cases), `evals/se_quality_cases.jsonl` (50 cases), `evals/se_realworld_cases.jsonl` (100 cases).
- Quality judge: GLM 5.2 scores correctness, completeness, and relevance (0.0-1.0).
- Metrics: quality mean, pass rate (≥0.7), classification accuracy, per-tier accuracy, p50/p95/p99 latency, total tokens, tokens per quality point, cost per quality point, cache hit rate.
- Benchmark runners: `scripts/se_benchmark.py`, `scripts/completion_quality_vps.py`, `scripts/benchmark_vps.py`.
- Strategies compared: Miser Auto, OpenRouter Auto, GPT-4.1-mini (fixed), GLM 5.2 (fixed), Claude Sonnet 4 (fixed).
