# Evaluation Methodology

## Corpus

`evals/cases.jsonl` is versioned and contains labeled OpenAI-compatible requests. Labels are not included in the classifier input. Add cases for coding, general chat, ambiguity, tool use, structured output, long context, adversarial keywords, multi-turn context, and overrides.

`evals/classifier_cases.jsonl` is the exhaustive prompt-classification corpus (116 cases): per-tier coverage, lexical traps that embed tier keywords in low-work prompts, read-only vs mutating tool use, tool-history chains, multi-turn context, and structured-output requests. Use it for classifier benchmarking; `scripts/classifier_benchmark.sh` defaults to it.

Labeling doctrine for tiers: a request answerable by a single short factual sentence is `trivial` regardless of technical jargon it names; producing an artifact (regex, command, snippet) or explaining a concept is `simple`; multi-file feature/debug/infrastructure work is `standard`; system design at scale, incident analysis, threat models, and cross-service migrations are `hard`; formal proofs and derivations are `reasoning`.

### Jev tuning result (2026-09-20, model `jev-1.13.0`)

Tuning corpus: `evals/classifier_cases_large.jsonl` (2100 prompts across web, infra, data, security, SRE, and theory domains; generator: `scripts/generate_classifier_corpus.py`). Held-out: `evals/classifier_cases.jsonl` (116 cases, adversarial traps, tool use, multi-turn). Three rubric iterations on the tuning corpus only; the held-out set was never used for tuning. `evals/cases.jsonl` is the legacy heuristic-corpus: its labels predate the tier doctrine above (any agentic action is labeled hard) and it over-credits keyword matching — use the `classifier_cases*` corpora for classifier benchmarking.

| corpus | classifier | exact | adjacent | MAE | under | over | p50 ms | notes |
|---|---|---|---|---|---|---|---|---|
| tuning (2100) | heuristic | 0.789 | 0.937 | 0.289 | 0.106 | 0.106 | 0 | zero-cost |
| tuning (2100) | jev | 0.978 | 1.000 | 0.022 | 0.009 | 0.014 | 363 | ~2.0M tokens, ~$0.10/run |
| held-out (116) | heuristic | 0.741 | 0.905 | 0.388 | 0.095 | 0.164 | 0 | zero-cost |
| held-out (116) | jev | 0.905 | 1.000 | 0.095 | 0.060 | 0.034 | 336 | serial run, final shipping rubric |

Jev is the default classifier mode and dominates the zero-cost heuristic on every axis, including under-routing; the trade is ~340 ms p50 per classification. Tool context (names + history) is part of the Jev state, so agentic capability floors (read-only tool ops → standard, mutating/multi-step → hard) are model-judged, not regex-matched.

## Metrics

- Exact accuracy: predicted tier equals expected tier.
- Adjacent accuracy: ordinal tier distance is at most one.
- Confusion matrix: expected rows and predicted columns.
- Under-routing rate: predicted tier is below expected tier.
- Over-routing rate: predicted tier is above expected tier.
- Mean absolute tier distance.
- p50/p95/p99 classifier latency.
- Classifier failure and timeout rate.
- Estimated model cost using the configured route price table.

## Commands

```bash
cargo run -p miser-evals -- --mode heuristic --corpus evals/cases.jsonl
cargo run -p miser-evals -- --mode local_llm --corpus evals/cases.jsonl --config config/miser.toml
cargo run -p miser-evals -- --mode cloud_llm --corpus evals/cases.jsonl --config config/miser.toml
cargo run -p miser-evals -- --mode jev --corpus evals/cases.jsonl --config config/miser.toml
scripts/classifier_benchmark.sh evals/cases.jsonl heuristic local_llm cloud_llm hybrid jev
```

`--config` supplies endpoint settings (model, base URL, timeout, API key). Keys resolve from the environment when absent from the config: `JEV_API_KEY` for the Jev evaluation endpoint, `OPENROUTER_API_KEY` for `cloud_llm`. `scripts/classifier_benchmark.sh` prints a per-mode exact/adjacent accuracy and p50/avg latency comparison table.

Run each mode against the same shuffled corpus. Warm local services before measuring latency. Record model name, endpoint class, hardware, concurrency, timeout, and date.

## Benchmark rules

- Keep labels outside the model-visible request.
- Balance tiers and workload categories.
- Include short hard/reasoning prompts and long trivial/simple prompts.
- Include lexical traps so keyword matching is measurable rather than hidden.
- Keep valid overrides separate from semantic accuracy.
- Do not tune and report on the same hidden cases.
- Report failures separately from default-tier predictions.
- Treat under-routing as more dangerous than over-routing for production code, security, financial, and incident prompts.

## Interpretation

Accuracy alone does not establish cost savings. Compare quality and cost of routed completions against a fixed strong-model baseline. A classifier is useful only when its reduced model spend outweighs classification cost and quality-recovery retries. Keep a conservative default and provide explicit route overrides for users.
