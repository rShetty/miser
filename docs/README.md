# Miser Documentation

- [Routing](./ROUTING.md) — how a tier is chosen, and how each stage is verified
- [High-Level Design](./HLD.md)
- [Low-Level Design](./LLD.md)
- [Security Model](./SECURITY.md)
- [Operations Runbook](./OPERATIONS.md)
- [Evaluation Methodology](./EVALUATION.md)

Miser is an OpenAI-compatible gateway. The documentation describes the current Rust MVP and marks planned capabilities explicitly. The default classifier is **Jev** (TypeSafe System One evaluation model) — see [Evaluation Methodology](./EVALUATION.md) for the benchmark evidence and [Operations Runbook](./OPERATIONS.md) for key rotation and fallback detection. For installation, follow the copy-paste guide in [Setup Guide](./SETUP.md).

## Start here

If you are changing how requests are routed, read [Routing](./ROUTING.md) first.
It records the invariants that the rest of the codebase is built to hold —
notably that the cheap decision is a **floor** (verification and context signals
can raise a tier, never lower it) and that a security refusal must never be
swallowed by the availability fallback.

## Two things that are easy to get wrong

* **`usage.cost` does not exist on TypeSafe direct.** It is an OpenRouter billing
  addition. Classifier cost is computed locally from token counts, or not
  reported at all.
* **The corpora are not equally trustworthy.** `cases.jsonl` is hand-curated and
  held at 100% by CI. The other two are generated — the large one is 83.5%
  duplicate rows behind 347 unique prompts — and are held at accuracy floors
  instead, because demanding 100% of generated labels would encode label noise
  as truth.
