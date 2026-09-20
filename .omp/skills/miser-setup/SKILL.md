---
name: miser-setup
description: >
  Install and configure the Miser AI gateway (rShetty/miser) with Jev classification
  enabled. Use when the user asks to set up, install, deploy, or configure Miser,
  enable Jev / TypeSafe System One classification, wire an OpenAI-compatible coding
  agent (OpenCode, Codex, Aider) through a cost-saving gateway, or debug why Miser
  classifies prompts with the fallback heuristic. Triggers: miser, Jev, TypeSafe,
  AI gateway, model routing, classifier setup, cheapest model routing.
---

# Miser + Jev Setup Skill

Install the Miser gateway and enable **Jev** — TypeSafe's System One evaluation
model — as its default prompt classifier. After this skill completes, every
prompt sent through Miser is classified by one Jev evaluation call and routed to
the cheapest capable model, with graceful fallback to a zero-cost heuristic.

## Prerequisites

Check before starting; tell the user what is missing:

- Rust toolchain: `cargo --version` (install via [rustup](https://rustup.rs) if absent)
- Git: `git --version`
- The user's Jev API key — from the [TypeSafe Console](https://console.typesafe.ai/)
  (direct, recommended, model `jev-latest`) or a
  [Vercel AI Gateway](https://vercel.com/ai-gateway/models/jev) key
  (model `typesafe-ai/jev`)
- An OpenRouter key for the upstream models: `OPENROUTER_API_KEY` (ask the user; required to start the gateway)

## Step 1 — Store the Jev key in ~/.env

The key must live in `~/.env` (or the repo `.env`, which `start_server.sh` loads).
Never commit it, never echo its value into logs or transcripts.

```bash
# If ~/.env already contains JEV_API_KEY, keep it. Otherwise ask the user for
# the key and append it with restrictive permissions:
grep -q '^JEV_API_KEY=' ~/.env || echo "JEV_API_KEY=<key-from-user>" >> ~/.env
chmod 600 ~/.env
```

If the user's existing key is stored under another name (for example
`JEV_KEY=`), normalize it:

```bash
grep -q '^JEV_API_KEY=' ~/.env || \
  grep '^JEV_KEY=' ~/.env | sed 's/^JEV_KEY=/JEV_API_KEY=/' >> ~/.env
```

## Step 2 — Get and build Miser

```bash
if [ -d ~/Work/miser/.git ]; then
  git -C ~/Work/miser pull --ff-only
else
  git clone https://github.com/rShetty/miser.git ~/Work/miser
fi
cargo build --release --manifest-path ~/Work/miser/Cargo.toml
```

## Step 3 — Confirm Jev is the configured classifier

The shipped config already defaults to Jev (TypeSafe direct). Verify rather
than assume:

```bash
grep -E '^mode|^\[classifier.jev\]|^enabled|^base_url|^model' ~/Work/miser/config/miser.toml
```

Expected: `mode = "jev"` and `[classifier.jev]` with `enabled = true`,
`base_url = "https://api.typesafe.ai/v1"`, `path = "/systemone"`,
`model = "jev-latest"`. To use a Vercel AI Gateway key instead, change
`base_url` to `https://ai-gateway.vercel.sh/v1`, `path` to `/evaluate`, and
`model` to `typesafe-ai/jev`.

## Step 4 — Start and verify live classification

```bash
cd ~/Work/miser
# start_server.sh loads repo .env; mirror ~/.env into it if needed
grep -q '^JEV_API_KEY=' .env || grep '^JEV_API_KEY=' ~/.env >> .env
grep -q '^OPENROUTER_API_KEY=' .env || echo "OPENROUTER_API_KEY=<key-from-user>" >> .env
chmod 600 .env
./start_server.sh
```

Then prove classification actually goes through Jev (the `x-miser-classifier`
header must say `jev`, not `heuristic`):

```bash
source ~/.env
curl -sD - -o /dev/null http://127.0.0.1:8787/v1/chat/completions \
  -H "Authorization: Bearer local" -H "Content-Type: application/json" \
  -d '{"model":"auto","messages":[{"role":"user","content":"explain database indexes"}]}' \
  | grep -i x-miser-classifier
```

- `x-miser-classifier: jev` → done, Jev is live.
- `x-miser-classifier: heuristic` → the Jev call failed; check
  `server.log`, the key, and `[classifier.jev].timeout_ms` (raise from 3000 on
  slow networks). A missing key never breaks the gateway — it degrades to the
  heuristic by design.

## Step 5 — (Recommended) Verify classification quality

Run the benchmark harness with the held-out adversarial corpus; expected jev
exact accuracy ≥ 0.87, adjacent ≥ 0.99, zero failures:

```bash
cd ~/Work/miser && source ~/.env
scripts/classifier_benchmark.sh evals/classifier_cases.jsonl heuristic jev
```

## Step 6 — Point a coding agent at the gateway

OpenCode example (`~/.config/opencode/opencode.json`):

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

Any OpenAI-compatible client works: point `baseURL` at
`http://127.0.0.1:8787/v1` (or your deployed HTTPS host) and model `auto`.

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `x-miser-classifier: heuristic` under mode `jev` | missing/invalid key, endpoint down, timeout too tight | verify `~/.env` key works (see below), check `server.log`, raise `timeout_ms` |
| Gateway exits: "provider API key is required" | `OPENROUTER_API_KEY` not set | add it to `.env` and restart |
| Port 8787 in use | previous instance running | `kill $(cat server.pid)` then rerun `start_server.sh` |
| 401 from typesafe.ai | key revoked or wrong surface | direct keys need TypeSafe Console; Vercel keys need the gateway URL trio from Step 3 |

Key sanity check without leaking the secret:

```bash
source ~/.env
curl -s -o /dev/null -w '%{http_code}\n' https://api.typesafe.ai/v1/systemone \
  -H "Authorization: Bearer $JEV_API_KEY" -H "Content-Type: application/json" \
  -d '{"model":"jev-latest","state":{"request":"hi"},"questions":{"t":{"type":"choice","instructions":"tier?","criteria":{"trivial":"t","simple":"s","standard":"m","hard":"h","reasoning":"r"}}}}'
# 200 = key valid, 401 = key rejected
```

## Reference

- Repo: https://github.com/rShetty/miser
- Benchmark methodology and results: `docs/EVALUATION.md`
- Ops runbook (rotation, fallback detection, latency budget): `docs/OPERATIONS.md`
