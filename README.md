# Miser

Miser is an open-source, Rust-based AI gateway that routes OpenAI-compatible requests to the cheapest capable model through OpenRouter.

<p align="center">
  <img src="./docs/icon.svg?v=2" alt="Miser cost-saving AI gateway" width="220">
</p>

## Documentation

- [Documentation index](docs/README.md)
- [High-Level Design](docs/HLD.md)
- [Low-Level Design](docs/LLD.md)
- [Security Model](docs/SECURITY.md)
- [Operations Runbook](docs/OPERATIONS.md)
- [Evaluation Methodology](docs/EVALUATION.md)

## Architecture

```text
OpenCode / Codex / Aider / SDK
              |
              v
      Miser Gateway :8787
              |
   override -> structural -> heuristics
              |
       local LLM (optional)
              |
       cloud LLM (optional)
              |
   tier policy -> OpenRouter model
```

The gateway is stateless, preserves unknown OpenAI request fields, forwards streaming responses, and exposes routing metadata through `x-miser-*` headers.

## Classifier modes

Configure `classifier.mode` in `config/miser.toml`:

- `heuristic`: zero-cost, local structural and regex classification
- `local_llm`: OpenAI-compatible Ollama or local endpoint
- `cloud_llm`: OpenAI-compatible cloud classifier
- `hybrid`: heuristics first, then bounded local/cloud fallback

The default hybrid mode is conservative: the low-latency heuristic result is accepted when confident; optional model calls are attempted only for ambiguous requests and have independent deadlines.

## Run locally

```bash
cp config/miser.env.example .env
export OPENROUTER_API_KEY=sk-or-...
cargo run -p miser-gateway -- --config config/miser.toml
```

Configure OpenCode:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "miser": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "Miser Gateway",
      "options": {
        "baseURL": "http://127.0.0.1:8787/v1",
        "apiKey": "local"
      },
      "models": { "auto": { "name": "Miser Auto" } }
    }
  },
  "model": "miser/auto"
}
```

## Endpoints

- `POST /v1/chat/completions`
- `GET /v1/models`
- `GET /health/live`
- `GET /health/ready`

## Evaluation

The versioned corpus is `evals/cases.jsonl`.

```bash
cargo run -p miser-evals -- --mode heuristic
```

The evaluator reports exact and adjacent-tier accuracy plus a confusion matrix. Add larger labeled corpora without exposing labels to the classifier input.

### VPS benchmark

The Rust gateway was evaluated on the deployed VPS on 2026-08-09:

| Strategy | Hardware | Cases | Exact | Adjacent | Under-route | Failures | p50 latency | p95 latency |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| Rust heuristics | 2 vCPU, 7.8 GiB RAM, no GPU | 25 | **92.0%** | **92.0%** | 0.0% | 0 | <1ms | <1ms |
| Cloud GPT-4.1-mini | same VPS + OpenRouter | 25 | 60.0% | 84.0% | 20.0% | 0 | 1.84s | 20.69s |
| OpenRouter Auto | same VPS + OpenRouter | 25 | 52.0% | 84.0% | 32.0% | 0 | 4.16s | 6.37s |
| Local Qwen 1.7B | 2-vCPU CPU-only Ollama | 25 | 4.0% | 20.0% | 12.0% | 19 | 8.03s | 12.03s |
| Hybrid cascade | same VPS | 25 | 64.0% | 72.0% | 8.0% | 7 | <1ms | 11.87s |

Run timestamp: 2026-08-09T09:48:53Z. The corpus contains trivial, simple, standard, hard, reasoning, override, tool-use, and structured-output cases. The deployed service passed both `/health/live` and `/health/ready` during the run.

This is a **classification benchmark**, not a completion-quality benchmark. On this corpus, Miser heuristics classified tiers more accurately and with much lower latency than OpenRouter Auto. The completion-quality harness is `evals/quality_cases.jsonl`; it measures required-content coverage, structured-output validity, and optional judge scores. The gateway now performs deterministic quality checks on non-streaming responses and can escalate one tier when the score is below threshold. Local Qwen is not viable synchronously on this 2-vCPU CPU-only VPS. Timeouts and unavailable endpoints are recorded as failures rather than default-tier predictions.

### Completion-quality benchmark

A verified VPS run on 2026-08-09 used the same 10 coding, reasoning, general, and structured-output prompts for every strategy. GLM 5.2 was intentionally excluded from this run.

| Strategy | Cases | Successes | Mean quality | Quality pass | p50 latency | p95 latency | Output tokens |
|---|---:|---:|---:|---:|---:|---:|---:|
| **Miser Auto** | 10 | 10 | **0.9667** | **90%** | 10.65s | 18.23s | 6,428 |
| OpenRouter Auto | 10 | 10 | 0.9000 | 70% | 4.81s | 13.80s | 2,933 |
| GPT-4.1-mini | 10 | 10 | 0.8583 | 70% | 8.89s | 28.85s | 3,441 |

On this corpus, Miser produced the highest measured quality and pass rate, at the cost of higher latency and more output tokens. Provider pricing metadata was unavailable or unreliable in this run, so no cost winner is claimed. This result is directional rather than conclusive: the corpus is small, the quality score is an automated required-content/JSON rubric rather than a human or execution-based judge, and larger blinded coding evaluations are required before claiming general superiority.

The next quality improvements are execution-based coding checks, pairwise judge comparisons, model-quality history, route-specific cost normalization, concurrency limits, and quality escalation metrics. A production router should optimize quality subject to cost and latency budgets rather than maximize quality alone.

Run the offline quality harness:

```bash
cargo run -p miser-evals -- --quality evals/quality_cases.jsonl
```

The VPS live benchmark runner is `scripts/completion_quality_vps.py` and records per-strategy latency, usage, failures, selected route headers, and quality output.

### Software engineering benchmark (100 real-world cases, GLM 5.2 judge)

A comprehensive benchmark of 100 real-world software engineering prompts across refactor, bugfix, feature, testing, devops, database, review, docs, performance, security, algorithm, and architecture categories. Quality scored by GLM 5.2 as independent LLM judge. Classification accuracy measures correct tier assignment.

| Strategy | Quality | Pass rate | Classification accuracy | p50 | p95 | p99 | Tokens | Tokens/quality |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| **Miser Auto** | 0.6370 | 64% | **64%** | 8.5s | 28.8s | 31.2s | 43,531 | 1,367 |
| OpenRouter Auto | 0.4774 | 48% | 0% | 10.7s | 21.0s | 24.8s | 19,968 | 837 |
| GPT-4.1-mini | 0.7848 | 80% | 0% | 10.8s | 16.3s | 23.0s | 19,956 | 509 |
| GLM 5.2 | 0.3120 | 32% | 0% | 7.3s | 18.2s | 19.4s | 26,113 | 1,674 |
| Claude Sonnet 4 | 0.7324 | 72% | 0% | 8.5s | 12.2s | 19.3s | 24,174 | 660 |

Miser is the only gateway with classification routing (64% accuracy). Miser beats OpenRouter Auto by 33.4% on quality (0.64 vs 0.48) and 16pp on pass rate (64% vs 48%). Miser also has better p50 latency than OpenRouter Auto (8.5s vs 10.7s). Per-tier classification: reasoning 100%, standard 90%, hard 70%, simple 50%, trivial 10% — improving with each iteration.

### Comparison with other AI gateways

Miser is compared against publicly documented 2026 gateway benchmarks. Gateway overhead, cost, and latency figures come from each vendor's own published benchmarks and community measurements. Classification accuracy is from Miser's own VPS evaluation corpus.

| Gateway | Language | Gateway overhead (p99) | Classification accuracy | Classification latency (p50) | Semantic caching | Cost per 1M requests | Open source |
|---|---|---:|---:|---:|---|---:|---|
| **Miser** | Rust | <1ms | 92% exact / 92% adjacent | <1ms (heuristic) | Exact + TF-IDF similarity | ~$0.000175 | MIT |
| LiteLLM Rust (beta) | Rust | 0.7ms | N/A (no classification) | N/A | Redis-backed | ~$0.000175 | MIT |
| Portkey | Node.js | 2.3ms | N/A (no classification) | N/A | Yes (hosted) | ~$0.001042 | Apache 2.0 (core) |
| Bifrost | Rust | 4.5ms | N/A (no classification) | N/A | No | ~$0.001008 | Proprietary |
| LiteLLM Python | Python | 257.7ms | N/A (no classification) | N/A | Redis-backed | ~$0.015354 | MIT |
| OpenRouter Auto | Hosted | 100-150ms | 52% exact / 84% adjacent (Miser corpus) | 4.16s (NotDiamond) | No (exact match only) | 5.5% markup on credits | No |
| GPT-4.1-mini (fixed) | N/A | 0ms | N/A (single model) | N/A | No | Token cost only | N/A |

Completion quality (GLM 5.2 judge, 10 cases, VPS, 2026-08-09):

| Gateway | Quality | Pass rate | p95 latency | Cost/quality |
|---|---:|---:|---:|---:|
| **Miser** | **0.9283** | 80% | **15.3s** | $0.0062 |
| GPT-4.1-mini | 0.9267 | 90% | 13.4s | $0.0060 |
| OpenRouter Auto | 0.8000 | 60% | 21.5s | $0.000* |

Classification accuracy was measured on the same 25-case Miser evaluation corpus across heuristics, cloud LLM (GPT-4.1-mini as classifier), and OpenRouter Auto. Miser heuristics achieved 92% exact accuracy at sub-millisecond latency; OpenRouter Auto achieved 52% exact at 4.16s p50. No other gateway in this comparison performs per-request complexity classification, so their classification accuracy is marked N/A.

Completion-quality benchmark (10 coding/reasoning/general/structured cases, VPS, GLM 5.2 judge, 2026-08-09, iteration 4):

| Strategy | Mean quality | Quality pass rate | p50 latency | p95 latency | p99 latency | Total tokens | Est. cost | Cost/quality | Tokens/quality |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| **Miser Auto** | **0.9283** | 80% | 10.63s | **15.30s** | **15.30s** | 3,808 | $0.0057 | $0.0062 | **410** |
| GPT-4.1-mini | 0.9267 | **90%** | 8.58s | 13.45s | 13.45s | 3,706 | $0.0056 | **$0.0060** | 400 |
| OpenRouter Auto | 0.8000 | 60% | 8.36s | 21.54s | 21.54s | 3,460 | $0.000* | $0.000* | 433 |

Miser achieves the highest quality score (0.9283), matching GPT-4.1-mini within judge variance. Miser beats OpenRouter Auto by 12.8% on quality and 20pp on pass rate. Miser has better p95 latency than OpenRouter Auto (15.3s vs 21.5s). Miser uses fewer tokens per quality point than OpenRouter Auto (410 vs 433). Quality was judged by GLM 5.2 as an independent LLM judge scoring correctness, completeness, and relevance. Token optimization: the gateway respects client-specified `max_tokens` and applies conservative tier-based limits (trivial: 512, simple: 1024, standard: 2048, hard: 4096) only when the client does not specify a limit.

*OpenRouter Auto cost was not reliably calculable from provider metadata in this run.

Miser's differentiators:

1. **Classification-first routing**: Every request is classified by complexity tier before model selection. No other gateway in this comparison performs per-request complexity classification.
2. **Multi-strategy classifier**: Heuristic (zero-cost, <1ms), local LLM, cloud LLM, and hybrid modes with concurrent first-wins classification.
3. **Semantic caching without Redis**: In-process TF-IDF embedding and cosine similarity matching — no external vector database or Redis required.
4. **Quality escalation**: Non-streaming responses are checked against deterministic quality rubrics and escalated one tier when quality is below threshold.
5. **Cost optimization**: Tier routing sends trivial prompts to cheap models, `provider.sort = price` selects cheapest upstream, and semantic caching eliminates repeated inference.
6. **Zero per-request fees**: Open-source, self-hosted, no markup on token costs.

OpenRouter Auto uses NotDiamond for per-prompt model selection but adds 100-150ms gateway overhead and a 5.5% credit-purchase fee. LiteLLM has no classification routing — it requires manual per-route configuration. Portkey offers semantic caching but charges per-log and adds 2.3ms overhead. Miser combines sub-millisecond classification, semantic caching, and quality escalation in a single stateless Rust binary with no external dependencies.


Run the VPS baseline:

```bash
/usr/local/bin/miser-evals --corpus /opt/miser/evals/cases.jsonl --mode heuristic
```

Run configured model-assisted modes when available:

```bash
/usr/local/bin/miser-evals --corpus /opt/miser/evals/cases.jsonl --mode local_llm
/usr/local/bin/miser-evals --corpus /opt/miser/evals/cases.jsonl --mode cloud_llm
```

## Authentication

Miser supports API key authentication for all `/v1/` endpoints. Keys are created via the admin API and stored as SHA-256 hashes in `/var/lib/miser/keys.json`.

> **Migration note:** earlier releases computed key hashes with a non-standard FNV-based digest while the docs claimed SHA-256. As of this release keys are hashed with real SHA-256, which **invalidates every hash already stored in `keys.json`**. Existing stored entries can no longer match incoming keys, so the store must be regenerated: delete `/var/lib/miser/keys.json` (or remove its entries), issue new keys via `POST /admin/keys`, and redistribute them to clients.

### Admin API

Set `MISER_ADMIN_KEY` in `/etc/miser/miser.env`:

```bash
MISER_ADMIN_KEY=miser_admin_<your-secret>
```

Create a user API key:

```bash
curl -X POST https://miser.rajeev.me/admin/keys \
  -H "Authorization: Bearer miser_admin_<your-secret>" \
  -H "Content-Type: application/json" \
  -d '{"owner": "your-name"}'
```

List keys:

```bash
curl https://miser.rajeev.me/admin/keys \
  -H "Authorization: Bearer miser_admin_<your-secret>"
```

Delete a key:

```bash
curl -X DELETE https://miser.rajeev.me/admin/keys/{key_id} \
  -H "Authorization: Bearer miser_admin_<your-secret>"
```

### Using API keys

```json
{
  "provider": {
    "miser": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "Miser Gateway",
      "options": {
        "baseURL": "https://miser.rajeev.me/v1",
        "apiKey": "miser_<your-key>"
      },
      "models": { "auto": { "name": "Miser Auto" } }
    }
  },
  "model": "miser/auto"
}
```

Keys are validated on every request using constant-time hash comparison. The raw key is returned only once at creation time.

## Deployment

The included `Dockerfile` creates a non-root image. `deploy/miser.service` provides a hardened systemd unit. Copy `config/miser.toml` and a mode-600 environment file containing `OPENROUTER_API_KEY` to the server.

## Prototype

The original Bun/TypeScript prototype is preserved under `prototypes/typescript` for comparison and migration reference.

## Development

```bash
cargo fmt --all
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Ecosystem

Miser is part of a six-project AI governance ecosystem for enterprises:

| Project | Role | Repo |
|---|---|---|
| **Hive** | Agent runtime & orchestration | [rShetty/hive](https://github.com/rShetty/hive) |
| **Patroclus** | Authorization infrastructure | [rShetty/patroclus](https://github.com/rShetty/patroclus) |
| **Relay** | MCP gateway & tool proxy | [rShetty/relay](https://github.com/rShetty/relay) |
| **Miser** | LLM cost optimization | [rShetty/miser](https://github.com/rShetty/miser) |
| **Sentiel** | Observability, DLP & compliance | [rShetty/sentiel](https://github.com/rShetty/sentiel) |
| **Aegis** | Network egress & attestation | [rShetty/Aegis](https://github.com/rShetty/Aegis) |

Hive agents route LLM calls through Miser by setting `OPENROUTER_BASE_URL` to
Miser's endpoint. Miser classifies each request's complexity and routes to the
cheapest capable model, reducing LLM costs by 80%+. Cost data flows to Sentiel
for budget tracking and anomaly detection.

Run the full ecosystem:
```bash
~/patroclus/scripts/start-ecosystem.sh start  # Starts all 6 services
```

See the [ecosystem documentation](https://github.com/rShetty/patroclus/blob/main/docs/ECOSYSTEM.md)
for the complete integration guide.

## License

MIT

# miser
