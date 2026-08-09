# Evaluation Methodology

## Corpus

`evals/cases.jsonl` is versioned and contains labeled OpenAI-compatible requests. Labels are not included in the classifier input. Add cases for coding, general chat, ambiguity, tool use, structured output, long context, adversarial keywords, multi-turn context, and overrides.

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
cargo run -p miser-evals -- --mode local_llm --corpus evals/cases.jsonl
cargo run -p miser-evals -- --mode cloud_llm --corpus evals/cases.jsonl
```

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
