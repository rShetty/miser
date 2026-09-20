# Install Miser with Jev Classification (copy-paste guide)

Every block below is copy-paste-able, in order. At the end you have the Miser
gateway running locally with **Jev** (TypeSafe System One) classifying every
prompt and **auto mode** routing each one to the cheapest capable model.

## 0. Prerequisites

```bash
cargo --version || curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
git --version
```

You also need two API keys:

| Key | Where to get it | Used for |
|---|---|---|
| `JEV_API_KEY` | [TypeSafe Console](https://console.typesafe.ai/) (direct, recommended) or [Vercel AI Gateway](https://vercel.com/ai-gateway/models/jev) | classifying prompts |
| `OPENROUTER_API_KEY` | [OpenRouter](https://openrouter.ai/keys) | the upstream models being routed |

## 1. Clone and build

```bash
git clone https://github.com/rShetty/miser.git ~/Work/miser
cargo build --release --manifest-path ~/Work/miser/Cargo.toml
```

## 2. Store your keys in ~/.env

```bash
cat >> ~/.env <<'EOF'
JEV_API_KEY=paste-your-typesafe-key
OPENROUTER_API_KEY=paste-your-openrouter-key
EOF
chmod 600 ~/.env
```

Quick check that the Jev key works (expect `200`):

```bash
set -a; source ~/.env; set +a
curl -s -o /dev/null -w '%{http_code}\n' https://api.typesafe.ai/v1/systemone \
  -H "Authorization: Bearer $JEV_API_KEY" -H "Content-Type: application/json" \
  -d '{"model":"jev-latest","state":{"request":"hi"},"questions":{"t":{"type":"choice","instructions":"tier?","criteria":{"trivial":"t","simple":"s","standard":"m","hard":"h","reasoning":"r"}}}}'
```

## 3. Start the gateway

```bash
cd ~/Work/miser
cp ~/.env .env                # start_server.sh loads the repo .env
chmod 600 .env
./start_server.sh
curl -fsS http://127.0.0.1:8787/health/live
```

## 4. Prove classification goes through Jev

The `x-miser-classifier` header must say `jev` — if it says `heuristic`, see
troubleshooting below.

```bash
set -a; source ~/.env; set +a
curl -sD - -o /dev/null http://127.0.0.1:8787/v1/chat/completions \
  -H "Authorization: Bearer local" -H "Content-Type: application/json" \
  -d '{"model":"auto","messages":[{"role":"user","content":"explain database indexes"}]}' \
  | grep -iE 'x-miser-tier|x-miser-classifier'
```

Expected:

```text
x-miser-tier: simple
x-miser-classifier: jev
```

## 5. (Optional) Verify classification quality

```bash
cd ~/Work/miser && set -a && source ~/.env && set +a
scripts/classifier_benchmark.sh evals/classifier_cases.jsonl heuristic jev
```

Expected: Jev exact ≥ 0.87, adjacent ≥ 0.99, zero failures (methodology and
tuned numbers in [EVALUATION.md](EVALUATION.md)).

## 6. Point your coding agent at Miser (auto mode)

OpenCode — `~/.config/opencode/opencode.json`:

```json
{
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

Any OpenAI-compatible client works the same way: `baseURL` =
`http://127.0.0.1:8787/v1`, model `auto`. Every request is classified by Jev
(~340 ms p50, ≈ $0.05 per 1,000 prompts) and routed to the cheapest capable
tier — `trivial` prompts stop paying frontier-model prices.

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `x-miser-classifier: heuristic` under mode `jev` | missing/invalid key, endpoint down, timeout too tight | re-run the step-2 check, inspect `server.log`, raise `[classifier.jev].timeout_ms` (default 3000) |
| Gateway exits: "provider API key is required" | `OPENROUTER_API_KEY` not set | add it to `.env`, rerun `./start_server.sh` |
| Port 8787 already in use | previous instance | `kill $(cat server.pid) && rm server.pid`, rerun `./start_server.sh` |
| 401 from typesafe.ai | key revoked or wrong surface | direct keys are TypeSafe Console keys; Vercel keys need `base_url = "https://ai-gateway.vercel.sh/v1"`, `path = "/evaluate"`, `model = "typesafe-ai/jev"` in `config/miser.toml` |

A missing or failing Jev key never takes the gateway down — classification
degrades to the built-in zero-cost heuristic and `x-miser-classifier` flips to
`heuristic`, which is how you detect it.

## What was installed

- `~/Work/miser` — the gateway (release binary at `target/release/miser-gateway`)
- `~/.env` / `~/Work/miser/.env` — your keys (never committed)
- `server.log`, `server.pid` in `~/Work/miser` — runtime artifacts
- Stop: `kill $(cat ~/Work/miser/server.pid)`; restart: `./start_server.sh`
