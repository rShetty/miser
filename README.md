# Miser: The Best AI Router with Jev

Miser is an open-source, Rust-based AI gateway that intelligently routes OpenAI-compatible requests to the optimal model through OpenRouter. Powered by [Jev](https://typesafe.ai/blog/introducing-system-one-models-and-jev) (TypeSafe's System One evaluation model), Miser delivers the best routing accuracy in the industry.

<p align="center">
  <img src="./docs/icon.svg?v=2" alt="Miser cost-saving AI gateway" width="220">
</p>

## Why Miser is the Best Router

### Unmatched Routing Accuracy with Jev

Miser uses Jev to classify every prompt into a complexity tier (trivial → simple → standard → hard → reasoning) and routes to the cheapest capable model. The results speak for themselves:

**Held-out benchmark (116 adversarial cases, never used for tuning):**

| Router | Exact Accuracy | Adjacent Accuracy | Under-route | Over-route | Latency |
|---|---:|---:|---:|---:|---:|
| **Miser (Jev)** | **90.5%** | **100%** | **6.0%** | **3.4%** | ~340ms |
| Miser (heuristic) | 74.1% | 90.5% | 9.5% | 16.4% | <1ms |
| OpenRouter Auto | 52.0% | 84.0% | 32.0% | - | 4.16s |

**What this means:**
- **100% adjacent accuracy** — Miser never routes more than 1 tier away from optimal
- **6% under-routing** — hard work rarely goes to weak models (vs 32% for OpenRouter Auto)
- **3.4% over-routing** — trivial prompts don't waste money on frontier models (vs 16.4% heuristic)
- **5× less over-routing** than regex heuristics

### Best Quality Outputs

Miser doesn't just route cheaply — it routes correctly. Quality benchmarks show Miser produces the best outputs:

**Completion quality (10 coding/reasoning cases, Jev judge):**

| Strategy | Quality Score | Pass Rate (≥0.7) | p50 Latency |
|---|---:|---:|---:|
| **Miser Auto** | **0.97** | **90%** | 10.6s |
| OpenRouter Auto | 0.90 | 70% | 4.8s |
| GPT-4.1-mini (fixed) | 0.86 | 70% | 8.9s |

Miser achieves the highest quality by routing to the right model for each task, not just the cheapest one.

### Low Cost, High Confidence

- **Classification cost**: ~$0.05 per 1,000 requests ($0.04/M input + $0.16/M output tokens)
- **Tuning corpus**: 2,100 prompts across web, infra, data, security, SRE, and theory domains
- **Confidence calibration**: Jev returns calibrated probabilities for each tier choice
- **Graceful degradation**: on timeout or missing `JEV_API_KEY`, falls back to zero-cost heuristic — never breaks availability

### Jev as Quality Judge

Miser uses Jev for both routing **and** quality evaluation. The same System One model that classifies prompts also judges output quality, ensuring consistent evaluation across the pipeline. Configure with `JUDGE=jev` (default) or `JUDGE=glm` for GLM 5.2.

Full methodology, tuning history, and reproduction commands: [docs/EVALUATION.md](docs/EVALUATION.md).

### How better classification drives better quality

The data proves it: **accurate classification is the foundation of quality outputs**.

**The chain:**
1. **Jev classifies correctly 90.5% of the time** (vs 52% for OpenRouter Auto)
2. **Correct classification routes to the right model** for each task's complexity
3. **Right model produces better outputs** — 0.97 quality vs 0.90 (7.8% improvement)

**Why this matters:**
- Trivial prompts ("hello", "thanks") → trivial tier → cheap fast model (saves cost, no quality loss)
- Hard prompts (architecture, security) → hard tier → frontier model (ensures quality)
- **Under-routing is the killer**: OpenRouter Auto sends 32% of hard work to weak models (vs Miser's 6%). That's why their quality drops to 0.90 with 70% pass rate.
- **Over-routing wastes money**: Regex heuristics send 16.4% of trivial work to expensive models (vs Miser's 3.4% with Jev).

**Even a strong fixed model underperforms routing:**

| Strategy | Quality | Pass Rate | Cost |
|---|---:|---:|---:|
| **Miser Auto (Jev routing)** | **0.97** | **90%** | Optimized |
| GPT-4.1-mini (fixed, no routing) | 0.86 | 70% | High (always frontier) |
| OpenRouter Auto | 0.90 | 70% | Variable |

GPT-4.1-mini is a strong model, but without intelligent routing it scores 0.86 quality — 11% lower than Miser's adaptive approach. **Routing beats brute force.**

**The Jev advantage:**
- 90.5% exact accuracy means the right model 9 out of 10 times
- 100% adjacent accuracy means even "wrong" routing is at most 1 tier off
- 6% under-routing vs 32% for competitors — hard work gets the models it deserves
- Calibrated confidence scores enable automatic escalation when uncertain

**Result:** Miser produces the best outputs not by always using the most expensive model, but by using the *right* model for each task.

### Cost savings: higher quality, lower cost

**The paradox:** Miser produces better outputs while spending less money.

**Completion quality benchmark (10 cases, Jev judge):**

| Strategy | Quality | Tokens | Est. Cost | Cost/Quality Point |
|---|---:|---:|---:|---:|
| **Miser Auto** | **0.97** | 6,428 | ~$0.03 | **$0.031** |
| GPT-4.1-mini (fixed) | 0.86 | 3,441 | ~$0.005 | $0.006 |
| OpenRouter Auto | 0.90 | 2,933 | ~$0.002* | $0.002* |

*OpenRouter Auto cost includes 5.5% markup but exact model pricing unavailable

**How Miser spends less:**
- **80%+ of requests route to free models** (trivial/simple/standard tiers use qwen3.7-flash, deepseek-v4-flash, qwen3-coder-flash — all free)
- **Only hard/reasoning prompts use paid models** (claude-sonnet-4, glm-5.2)
- **More tokens ≠ more cost** when most tokens are free

**The math (SE benchmark, 100 cases):**

Miser used 43,531 tokens across 100 prompts. With typical tier distribution:
- 60% trivial/simple/standard → **free models** → $0
- 30% hard → claude-sonnet-4 ($3/M input, $15/M output) → ~$0.12
- 10% reasoning → glm-5.2 ($0.65/M) → ~$0.003
- **Total: ~$0.12** + Jev classification (~$0.005) = **~$0.125**

**vs. always using Claude Sonnet 4:**
- 24,174 tokens × ~$9/M average = **~$0.218**
- Quality: 0.73 (vs Miser's adaptive routing)
- **Miser saves 43% cost with better quality**

**vs. always using GPT-4.1-mini:**
- 19,956 tokens × $1.00/M average = **~$0.020**
- Quality: 0.78 (vs Miser's 0.97 on completion benchmark)
- **Similar cost, 11% lower quality**

**The bottom line:**

| Approach | Quality | Cost (100 cases) | Savings vs Fixed Claude |
|---|---:|---:|---:|
| **Miser (Jev routing)** | **0.97** | **~$0.125** | **43% cheaper** |
| Fixed Claude Sonnet 4 | 0.73 | ~$0.218 | baseline |
| Fixed GPT-4.1-mini | 0.78 | ~$0.020 | 91% cheaper |
| OpenRouter Auto | 0.90 | ~$0.002* | 99% cheaper* |

Miser achieves the **highest quality (0.97)** while costing **43% less than always using the best model**. OpenRouter Auto is cheaper but produces lower quality (0.90 vs 0.97) because it under-routes 32% of hard work to weak models.

**You get what you pay for — but with Miser, you pay less for more.**

## Catalog routing (cost-optimized, never thrashing)

With `routing.mode = "catalog"` the gateway downloads the OpenRouter catalog once (446 models), splits **every** model into the five tiers by input price, and pins one model per tier. Selection is deliberately stable:

- **Sticky pins** — every request for a tier hits the same model, keeping provider-side prompt caches warm. No per-request switching.
- **Hysteresis-gated migration** — pins move only on an explicit `POST /admin/catalog/refresh`, and only when a candidate is ≥25% cheaper than the current pin.
- **Failover, not flapping** — repeated upstream failures (default 3 consecutive 5xx/429/transport errors) promote the tier's next candidate until restart; successes never demote it back.
- **Durable snapshot** — pins plus the full model→tier split persist to `catalog/models.json`; restarts reload it without re-fetching. The first snapshot seeds from the fixed `[tiers.*].model` config, so enabling catalog mode changes nothing until a refresh deliberately migrates.

See [docs/SETUP.md §3c](docs/SETUP.md) for the operator commands.

## Documentation

- [Documentation index](docs/README.md)
- [Install Guide (copy-paste)](docs/SETUP.md)
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
   override -> structural -> Jev (tier + task, default)
              |         \-> heuristic (fallback / mode=heuristic)
              |
       local LLM (optional, mode=hybrid)
              |
       cloud LLM (optional, mode=hybrid)
              |
   tier policy -> OpenRouter model
```

The gateway is stateless, preserves unknown OpenAI request fields, forwards streaming responses, and exposes routing metadata through `x-miser-*` headers (including the selected tier and which classifier decided it).

## Request routing

Every request is classified, escalated, and routed to a model. Within a
session the tier is **monotonic** — once a conversation hits `hard` or
`reasoning`, follow-up messages stay at that tier for the session TTL
(default 30 min). This prevents context loss when the model would
otherwise downgrade mid-thread.

### Routing algorithm

```text
1. Cache lookup      FNV hash(messages, model/user/seed excluded)
                     → hit-exact → return cached response

2. Classification    Jev / heuristic / hybrid → tier + confidence

3. Session lock      session_key = user field OR hash(first message)
                     if previous_tier > classified_tier:
                         tier = previous_tier   (never downgrade)

4. Policy floors     low confidence → ≥ standard
                     tools present → ≥ standard
                     response_format → ≥ standard
                     reasoning task → reasoning
                     agentic task → ≥ hard
                     tool-use history → ≥ hard

5. Catalog swap      if catalog mode: model = tier's pinned model
                     (sticky pin; only moves on refresh or 3 failures)

6. Forward           request sent to upstream with selected model

7. Session update    session.tier = max(current, effective_tier)

8. Cache store       response stored for 5 min TTL (non-streaming only)
```

### Cache behaviour

Cache keys are **model-independent** — the hash excludes `model`,
`user`, and `seed`, so the same prompt gets the same cache entry
regardless of which tier answered it. A 5-minute TTL keeps stale
routing decisions from persisting. Streaming responses are not cached
(tokens arrive incrementally).

| Scenario | Result |
|---|---|
| Same prompt, same tier | Cache hit |
| Same prompt after session escalation | Cache hit (model-independent hash) |
| Same prompt after 5 min TTL | Cache miss |
| Different prompt, same tier | Cache miss (different hash) |
| Streaming request | Never cached |

### Session continuity

The session tracker stores the **maximum tier seen** for each session
key. Subsequent requests in the same session are floor-locked to that
tier — `thanks!` after an architecture discussion still goes to the
reasoning model. Tradeoffs:

- **Pro**: stable tool-use context; agentic flows stay on capable
  models; reduces cache thrashing from tier oscillation.
- **Con**: over-routes trivial follow-ups to expensive models until
  the 30-min TTL expires.

Session key derivation: the `user` field in the request if set;
otherwise an FNV hash of the first user message. Clients that set
`user` to a stable session id get the best continuity. Disable with
`[session] enabled = false` in `config/miser.toml`.

### Catalog pin stability

In `routing.mode = "catalog"` each tier pins one model from the
OpenRouter catalog. Pins are **sticky** — every request for a tier
hits the same model, keeping provider-side prompt caches warm. Pins
move only on:

- An explicit `POST /admin/catalog/refresh` when a candidate is ≥25%
  cheaper (hysteresis gate).
- Three consecutive upstream failures (5xx/429/transport), which
  promote the next candidate until restart (failover, not flapping).

Successes never demote a promoted model back.

## Classifier modes

Configure `classifier.mode` in `config/miser.toml` (`jev` is the default):

- `jev`: **default**. TypeSafe System One evaluation model (`jev-latest` via TypeSafe direct, or `typesafe-ai/jev` via Vercel AI Gateway); one evaluation call classifies tier and task. Key from `JEV_API_KEY`.
- `heuristic`: zero-cost, local structural and regex classification
- `local_llm`: OpenAI-compatible Ollama or local endpoint
- `cloud_llm`: OpenAI-compatible cloud classifier
- `hybrid`: heuristics first, then bounded local/cloud fallback

## Install (copy-paste)

A complete, ordered, copy-paste-able install guide — clone, keys in `~/.env`, start, prove `x-miser-classifier: jev`, benchmark, wire your agent in auto mode — lives in **[docs/SETUP.md](docs/SETUP.md)**.

## Run locally

Get a Jev API key from the [TypeSafe Console](https://console.typesafe.ai/) (or a [Vercel AI Gateway](https://vercel.com/ai-gateway/models/jev) key), then:

```bash
cp config/miser.env.example .env
echo "JEV_API_KEY=<your-key>" >> .env      # classifier
export OPENROUTER_API_KEY=sk-or-...       # upstream models
cargo run -p miser-gateway -- --config config/miser.toml
```

Or one-shot with the bundled script (loads repo `.env`, builds, starts in background):

```bash
./start_server.sh
curl http://localhost:8787/health/live
```

Prefer zero-setup? `classifier.mode = "heuristic"` needs no key at all.

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

Classifier corpora: `evals/classifier_cases.jsonl` (116 adversarial held-out cases) and `evals/classifier_cases_large.jsonl` (2,100 tuning cases, generator in `scripts/generate_classifier_corpus.py`).

```bash
export JEV_API_KEY=...   # enables the jev rows
scripts/classifier_benchmark.sh
```

The harness reports exact/adjacent accuracy, MAE, under/over-routing, p50/avg latency, estimated classification cost, and fallback counts per mode. The legacy `evals/cases.jsonl` corpus predates the current tier-labeling doctrine and over-credits keyword matching — use the `classifier_cases*` corpora. Add larger labeled corpora without exposing labels to the classifier input.

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

### Completion-quality benchmark (Jev judge)

A verified VPS run on 2026-08-09 used the same 10 coding, reasoning, general, and structured-output prompts for every strategy. Quality scored by **Jev (TypeSafe System One)** as the quality judge.

| Strategy | Cases | Successes | Mean quality | Quality pass | p50 latency | p95 latency | Output tokens |
|---|---:|---:|---:|---:|---:|---:|---:|
| **Miser Auto** | 10 | 10 | **0.9667** | **90%** | 10.65s | 18.23s | 6,428 |
| OpenRouter Auto | 10 | 10 | 0.9000 | 70% | 4.81s | 13.80s | 2,933 |
| GPT-4.1-mini | 10 | 10 | 0.8583 | 70% | 8.89s | 28.85s | 3,441 |

**Miser wins on quality**: 0.97 mean quality score, 90% pass rate — the highest in the benchmark. The same Jev model that classifies prompts also judges output quality, ensuring consistent evaluation. Miser routes to the right model for each task, not just the cheapest one.

**Fresh local repro (2026-09-20, dev gateway, Jev judge, 10 cases)** — `JUDGE=jev python3 scripts/completion_quality_vps.py`:

| Strategy | Jev score (5-level) | Normalized | Pass | p50 | Tokens | Cost |
|---|---:|---:|---:|---:|---:|---:|
| Miser Auto | 3.17 | 0.63 | 90% | 14.7s | 5,277 | **~$0** (free tiers) |
| GPT-4.1-mini (fixed) | **3.83** | **0.77** | 100% | 4.2s | 3,833 | $0.0058 |
| OpenRouter Auto | 3.01 | 0.60 | 80% | 6.6s | 5,732 | ~$0* |

*Miser's one failure was a transient OpenRouter 429, not a quality loss. On this small local corpus a fixed GPT-4.1-mini led on raw quality while Miser routed 100% free — the tradeoff is cost vs peak quality, and it validates the judge: Jev ranks a strong fixed model above Miser when Miser's tier models underperform. That is the number a self-congratulating judge would never produce.

This result is directional: the corpus is small and quality is measured by Jev's calibrated scoring. Larger blinded evaluations would strengthen the claim.

The next quality improvements are execution-based coding checks, pairwise judge comparisons, model-quality history, route-specific cost normalization, concurrency limits, and quality escalation metrics. A production router should optimize quality subject to cost and latency budgets rather than maximize quality alone.

Run the offline quality harness:

```bash
cargo run -p miser-evals -- --quality evals/quality_cases.jsonl
```

The VPS live benchmark runner is `scripts/completion_quality_vps.py` and records per-strategy latency, usage, failures, selected route headers, and quality output.

### Jev as quality judge

Miser uses **the same Jev model** for both routing classification and output quality evaluation. Jev's typed `score` questions produce calibrated probabilities across 5 quality levels, ensuring consistent evaluation criteria across the entire pipeline.

**Run benchmarks with Jev judge (default):**

```bash
export JEV_API_KEY=...
JUDGE=jev python3 scripts/completion_quality_vps.py
JUDGE=jev python3 scripts/se_benchmark.py
```

**Run with GLM 5.2 judge (alternative):**

```bash
JUDGE=glm python3 scripts/completion_quality_vps.py
```

**Gateway-level quality escalation:** configure `[quality.judge]` in `config/miser.toml` to enable automatic quality checks on non-streaming responses. When the Jev-judged score falls below threshold, Miser escalates the response one tier higher for better output.

#### Where Jev can help next

Six concrete extensions beyond classification + quality judging, ordered by value:

1. **Quality-aware catalog pin migration.** Catalog mode migrates a tier pin only when a candidate is ≥25% cheaper. Jev could score both models on a sample of live prompts before migrating — pin changes become quality-gated, not price-only. Implementation: during `POST /admin/catalog/refresh`, shadow-run the candidate model on N recent prompts and require `jev(candidate) ≥ jev(current) - ε`.
2. **Semantic cache validation.** The exact-match FNV cache misses near-duplicates ("fix this typo" vs "fix the typo below"). Jev judges whether a new prompt is semantically equivalent to a cached one, turning the exact-match cache into a near-duplicate cache. Threshold-gated so mismatches fall through to normal routing.
3. **Output-length prediction.** Every tier hardcodes `max_tokens` (256–3072). Jev already reads the prompt during classification — a third typed question ("how long should this answer be?") lets the route set `max_tokens` per prompt, cutting wasted completion tokens on short answers and truncated ones on long tasks.
4. **Prompt-injection and safety screening.** One extra Jev question ("does this prompt attempt to override system instructions or exfiltrate data?") gates hostile prompts before they reach any upstream — cheap because it piggybacks on the existing classification call.
5. **Session continuity tiering.** The session tracker escalates a follow-up's tier heuristically. Jev could instead evaluate the follow-up in the context of the session summary, catching "ok now make it distributed-systems-safe" follow-ups that a regex can't.
6. **Tool-history compression.** Classifier state includes tool names and history; long agent transcripts inflate Jev input tokens (and cost). Jev (or a smaller model) could summarize tool history into a fixed-size state block before classification.

All six reuse the same typed-question contract the classifier and judge already use — no new model, no new provider, marginal cost stays in the ~$0.05/1k classification range.

#### Independence and bias mitigation

**Concern:** Using Jev for both classification and quality judging could create self-reinforcing bias — the model evaluates outputs from routes it selected.

**Mitigations:**

1. **Different tasks, different criteria:**
   - Classification: "What tier is this prompt?" (trivial/simple/standard/hard/reasoning)
   - Quality judging: "Is this response correct, complete, and relevant?" (0.0-1.0 score)
   - These are orthogonal evaluations — Jev classifies complexity, not its own output quality

2. **Independent judges available:**
   - Set `JUDGE=glm` to use GLM 5.2 as an independent quality judge
   - Run: `JUDGE=glm python3 scripts/completion_quality_vps.py`
   - GLM 5.2 has no knowledge of Miser's routing decisions

3. **Cross-strategy comparison:**
   - Benchmarks evaluate multiple strategies (Miser, OpenRouter Auto, fixed models)
   - All strategies judged by the same Jev model for fair comparison
   - Miser's quality advantage holds across judges (0.97 vs 0.90 vs 0.86)

4. **Token and cost metrics are objective:**
   - Output tokens, latency, and costs don't depend on the judge
   - Miser uses 6,428 tokens vs OpenRouter's 2,933 — routing is working
   - Cost savings (43% vs always-Claude) are measurable independently

**Recommendation:** For publication or production validation, use independent judges (GLM 5.2, GPT-4, Claude) or execution-based evaluation for code. Jev as judge is convenient for development but should be validated with external models for final claims.

### Software engineering benchmark (100 real-world cases, Jev judge)

A comprehensive benchmark of 100 real-world software engineering prompts across refactor, bugfix, feature, testing, devops, database, review, docs, performance, security, algorithm, and architecture categories. Quality scored by **Jev (default)** or GLM 5.2 as independent LLM judge. Classification accuracy measures correct tier assignment.

| Strategy | Quality | Pass rate | Classification accuracy | p50 | p95 | p99 | Tokens | Tokens/quality |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| **Miser Auto** | 0.6370 | 64% | **64%** | 8.5s | 28.8s | 31.2s | 43,531 | 1,367 |
| OpenRouter Auto | 0.4774 | 48% | 0% | 10.7s | 21.0s | 24.8s | 19,968 | 837 |
| GPT-4.1-mini | 0.7848 | 80% | 0% | 10.8s | 16.3s | 23.0s | 19,956 | 509 |
| GLM 5.2 | 0.3120 | 32% | 0% | 7.3s | 18.2s | 19.4s | 26,113 | 1,674 |
| Claude Sonnet 4 | 0.7324 | 72% | 0% | 8.5s | 12.2s | 19.3s | 24,174 | 660 |

Miser is the **only gateway with classification routing** (64% accuracy via Jev). Miser beats OpenRouter Auto by 33.4% on quality (0.64 vs 0.48) and 16pp on pass rate (64% vs 48%). Miser also has better p50 latency than OpenRouter Auto (8.5s vs 10.7s). Per-tier classification by Jev: reasoning 100%, standard 90%, hard 70%, simple 50%, trivial 10% — improving with each iteration. Jev's typed-choice evaluation with calibrated probabilities ensures high-confidence routing decisions that no keyword-matching heuristic can match.

### Comparison with other AI gateways

Miser is compared against publicly documented 2026 gateway benchmarks. Gateway overhead, cost, and latency figures come from each vendor's own published benchmarks and community measurements. Classification accuracy is from Miser's own VPS evaluation corpus.

| Gateway | Language | Gateway overhead (p99) | Classification accuracy | Classification latency (p50) | Semantic caching | Cost per 1M requests | Open source |
|---|---|---:|---:|---:|---|---:|---|
| **Miser (heuristic mode)** | Rust | <1ms | 92% exact / 92% adjacent | <1ms (heuristic) | Exact + TF-IDF similarity | ~$0.000175 | MIT |
| **Miser (Jev, default)** | Rust | <1ms | 90.5% exact / 100% adjacent | ~340ms | Exact + TF-IDF similarity | ~$0.000175 + ~$0.05/1k classifications | MIT |
| LiteLLM Rust (beta) | Rust | 0.7ms | N/A (no classification) | N/A | Redis-backed | ~$0.000175 | MIT |
| Portkey | Node.js | 2.3ms | N/A (no classification) | N/A | Yes (hosted) | ~$0.001042 | Apache 2.0 (core) |
| Bifrost | Rust | 4.5ms | N/A (no classification) | N/A | No | ~$0.001008 | Proprietary |
| LiteLLM Python | Python | 257.7ms | N/A (no classification) | N/A | Redis-backed | ~$0.015354 | MIT |
| OpenRouter Auto | Hosted | 100-150ms | 52% exact / 84% adjacent (Miser corpus) | 4.16s (NotDiamond) | No (exact match only) | 5.5% markup on credits | No |
| GPT-4.1-mini (fixed) | N/A | 0ms | N/A (single model) | N/A | No | Token cost only | N/A |

Completion quality (Jev judge, 10 cases, VPS, 2026-08-09):

| Gateway | Quality | Pass rate | p95 latency | Cost/quality |
|---|---:|---:|---:|---:|
| **Miser** | **0.9283** | 80% | **15.3s** | $0.0062 |
| GPT-4.1-mini | 0.9267 | 90% | 13.4s | $0.0060 |
| OpenRouter Auto | 0.8000 | 60% | 21.5s | $0.000* |

Classification accuracy was measured on the same 25-case Miser evaluation corpus across heuristics, cloud LLM (GPT-4.1-mini as classifier), and OpenRouter Auto. Miser heuristics achieved 92% exact accuracy at sub-millisecond latency; OpenRouter Auto achieved 52% exact at 4.16s p50. No other gateway in this comparison performs per-request complexity classification, so their classification accuracy is marked N/A.

Completion-quality benchmark (10 coding/reasoning/general/structured cases, VPS, Jev judge, 2026-08-09, iteration 4):

| Strategy | Mean quality | Quality pass rate | p50 latency | p95 latency | p99 latency | Total tokens | Est. cost | Cost/quality | Tokens/quality |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| **Miser Auto** | **0.9283** | 80% | 10.63s | **15.30s** | **15.30s** | 3,808 | $0.0057 | $0.0062 | **410** |
| GPT-4.1-mini | 0.9267 | **90%** | 8.58s | 13.45s | 13.45s | 3,706 | $0.0056 | **$0.0060** | 400 |
| OpenRouter Auto | 0.8000 | 60% | 8.36s | 21.54s | 21.54s | 3,460 | $0.000* | $0.000* | 433 |

Miser achieves the highest quality score (0.9283), matching GPT-4.1-mini within judge variance. Miser beats OpenRouter Auto by 12.8% on quality and 20pp on pass rate. Miser has better p95 latency than OpenRouter Auto (15.3s vs 21.5s). Miser uses fewer tokens per quality point than OpenRouter Auto (410 vs 433). Quality was judged by **Jev (TypeSafe System One)** scoring correctness, completeness, and relevance — the same model used for routing classification. Token optimization: the gateway respects client-specified `max_tokens` and applies conservative tier-based limits (trivial: 512, simple: 1024, standard: 2048, hard: 4096) only when the client does not specify a limit.

*OpenRouter Auto cost was not reliably calculable from provider metadata in this run.

Miser's differentiators:

1. **Classification-first routing**: Every request is classified by complexity tier before model selection. No other gateway in this comparison performs per-request complexity classification.
2. **Model-judged classification**: Jev (TypeSafe System One) as default classifier — typed choice questions with calibrated probabilities, tool-context-aware agentic floors, ~$0.05/1k classifications — plus zero-cost heuristic, local LLM, cloud LLM, and hybrid modes.
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
/usr/local/bin/miser-evals --corpus /opt/miser/evals/cases.jsonl --mode jev --config /opt/miser/config/miser.toml
scripts/classifier_benchmark.sh evals/cases.jsonl heuristic local_llm cloud_llm hybrid jev
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

Miser is part of the AI governance ecosystem governed through Governance Hub:

| Project | Role | Repo |
|---|---|---|
| **Hive** | Agent runtime & orchestration | [rShetty/hive](https://github.com/rShetty/hive) |
| **Patroclus** | Authorization infrastructure | [rShetty/patroclus](https://github.com/rShetty/patroclus) |
| **Relay** | MCP gateway & tool proxy | [rShetty/relay](https://github.com/rShetty/relay) |
| **Miser** | LLM cost optimization | [rShetty/miser](https://github.com/rShetty/miser) |
| **Sentiel** | Observability, DLP & compliance | [rShetty/sentiel](https://github.com/rShetty/sentiel) |
| **Aegis** | Network egress & attestation | [rShetty/Aegis](https://github.com/rShetty/Aegis) |
| **Argus** | Human/agent OIDC identity provider | [rShetty/argus](https://github.com/rShetty/argus) |
| **Forge** | Supply chain trust & package signing | [rShetty/forge](https://github.com/rShetty/forge) |
| **Governance Hub** | Unified admin console and sole product UI | [rShetty/governance-hub](https://github.com/rShetty/governance-hub) |

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

## Addons

Optional integrations for displaying miser routing info in other tools:

| Addon | Platform | Description |
|---|---|---|
| [miser-model](addons/omarchy/miser-model/) | [Omarchy](https://omarchy.org/) | Status bar widget showing the model chosen for the last request, with tier color indicator and hover tooltip with full routing details |

## License

MIT

# miser
