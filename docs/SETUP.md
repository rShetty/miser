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
# Admin + key-store paths (repo .env, used by start_server.sh)
cd ~/Work/miser
grep -q MISER_ADMIN_KEY .env 2>/dev/null || printf 'MISER_ADMIN_KEY=%s\n' "$(openssl rand -hex 24)" >> .env
grep -q MISER_KEYS_FILE .env 2>/dev/null || echo 'MISER_KEYS_FILE=/home/'"$(whoami)"'/.config/miser/keys.json' >> .env
mkdir -p ~/.config/miser
chmod 600 .env
```

> Setting `MISER_ADMIN_KEY` switches the gateway from open access to
> authenticated: every completion request needs an API key (created in
> step 3), and admin endpoints (catalog refresh, key management) need the
> admin key. Leave `MISER_ADMIN_KEY` unset only if you want a fully open
> local gateway — then `/admin/*` is disabled (always 401).

Quick check that both keys work (expect `200` from each):

```bash
set -a; source ~/.env; set +a
curl -s -o /dev/null -w 'jev: %{http_code}\n' https://api.typesafe.ai/v1/systemone \
  -H "Authorization: Bearer $JEV_API_KEY" -H "Content-Type: application/json" \
  -d '{"model":"jev-latest","state":{"request":"hi"},"questions":{"t":{"type":"choice","instructions":"tier?","criteria":{"trivial":"t","simple":"s","standard":"m","hard":"h","reasoning":"r"}}}}'
curl -s -o /dev/null -w 'openrouter: %{http_code}\n' https://openrouter.ai/api/v1/auth/key \
  -H "Authorization: Bearer $OPENROUTER_API_KEY"
```

If the OpenRouter check prints `401` — or sourcing `~/.env` prints
`command not found: sk-or-v1-…` — the key line is malformed. Each line must be
exactly `KEY=value` with no spaces or prefixes: a line like
`OPENROUTER_API_KEY=openrouter sk-or-v1-…` silently fails to set the variable
(the shell instead tries to execute everything after the space).

## 3. Start the gateway

```bash
cd ~/Work/miser
cp ~/.env .env                # start_server.sh loads the repo .env
grep -q MISER_ADMIN_KEY .env || printf 'MISER_ADMIN_KEY=%s\n' "$(openssl rand -hex 24)" >> .env
grep -q MISER_KEYS_FILE .env || echo "MISER_KEYS_FILE=$HOME/.config/miser/keys.json" >> .env
mkdir -p ~/.config/miser && chmod 600 .env
./start_server.sh
curl -fsS http://127.0.0.1:8787/health/live
```

## 3b. Create an API key for your clients

With `MISER_ADMIN_KEY` set, completion requests must carry a gateway API key:

```bash
cd ~/Work/miser && set -a && source .env && set +a
curl -s -X POST http://127.0.0.1:8787/admin/keys \
  -H "Authorization: Bearer $MISER_ADMIN_KEY" -H "Content-Type: application/json" \
  -d '{"owner":"local","client":"omp"}' | jq -r .key
# → miser_…  (shown once — store it in ~/.env as MISER_API_KEY=…)
```

## 3c. Catalog routing (model selection)

In `catalog` mode (default in `config/miser.toml`) the gateway splits every
OpenRouter model into the five complexity tiers by input price, pins one
model per tier, and **sticks to it** — no per-request model switching:

```bash
cd ~/Work/miser && set -a && source .env && set +a
# Fetch the catalog, re-partition, migrate pins (hysteresis-gated):
curl -s -X POST http://127.0.0.1:8787/admin/catalog/refresh \
  -H "Authorization: Bearer $MISER_ADMIN_KEY" | jq
# Inspect pins, active models, failover counters, tier split:
curl -s http://127.0.0.1:8787/admin/catalog \
  -H "Authorization: Bearer $MISER_ADMIN_KEY" | jq
```

Stability rules: pins change only on an explicit refresh and only when a
candidate is ≥25% cheaper (`routing.switch_saving_ratio`); repeated upstream
failures (default 3 consecutive 5xx/429/transport errors) fail a tier over
to its next candidate until restart; the snapshot persists to
`catalog/models.json` and survives restarts without re-fetching.

## 4. Prove classification goes through Jev

The `x-miser-classifier` header must say `jev` — if it says `heuristic`, see
troubleshooting below.

```bash
set -a; source ~/.env; set +a
curl -sD - -o /dev/null http://127.0.0.1:8787/v1/chat/completions \
  -H "Authorization: Bearer $MISER_API_KEY" -H "Content-Type: application/json" \
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

OpenCode — `~/.config/opencode/opencode.json` (`apiKey` is the gateway key
from step 3b):

```json
{
  "provider": {
    "miser": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "Miser Gateway",
      "options": {
        "baseURL": "http://127.0.0.1:8787/v1",
        "apiKey": "miser_…"
      },
      "models": { "auto": { "name": "Miser Auto" } }
    }
  },
  "model": "miser/auto"
}
```

OMP — `~/.omp/agent/models.yml` (`MISER_API_KEY` lives in `~/.env`; omp
resolves env-var names in `apiKey`):

```yaml
providers:
  miser:
    baseUrl: http://127.0.0.1:8787/v1
    api: openai-completions
    apiKey: MISER_API_KEY
    authHeader: true # send Authorization: Bearer <resolved key>
    models:
      - id: auto
        name: Miser Auto
        contextWindow: 200000 # largest routed tier (claude-sonnet-4)
        maxTokens: 8192
```

Then launch `omp --model miser/auto` (check with `omp models find miser`).
Only **new** sessions pick this up — running sessions keep their startup
catalog.

Any OpenAI-compatible client works the same way: `baseURL` =
`http://127.0.0.1:8787/v1`, model `auto`. Every request is classified by Jev
(~340 ms p50, ≈ $0.05 per 1,000 prompts) and routed to the cheapest capable
tier — `trivial` prompts stop paying frontier-model prices.

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `x-miser-classifier: heuristic` under mode `jev` | missing/invalid key, endpoint down, timeout too tight | re-run the step-2 check, inspect `server.log`, raise `[classifier.jev].timeout_ms` (default 3000) |
| OpenRouter key check prints `401` or sourcing `~/.env` errors `command not found: sk-or-v1-…` | key line has a space or stray prefix (e.g. `OPENROUTER_API_KEY=openrouter sk-or-v1-…`) | fix the line to exactly `OPENROUTER_API_KEY=sk-or-v1-…`, re-source, re-check |
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
