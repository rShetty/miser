# Miser

Miser is an open-source, Rust-based AI gateway that routes OpenAI-compatible requests to the cheapest capable model through OpenRouter.

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
