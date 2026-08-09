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

Run the VPS baseline:

```bash
/usr/local/bin/miser-evals --corpus /opt/miser/evals/cases.jsonl --mode heuristic
```

Run configured model-assisted modes when available:

```bash
/usr/local/bin/miser-evals --corpus /opt/miser/evals/cases.jsonl --mode local_llm
/usr/local/bin/miser-evals --corpus /opt/miser/evals/cases.jsonl --mode cloud_llm
```

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

## License

MIT

# miser
