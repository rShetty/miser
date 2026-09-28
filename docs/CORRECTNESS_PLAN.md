# Formal correctness pass

**Pass 1 complete, pass 2 in progress.** 15 defects found by structured review,
reproduced as failing tests, fixed, and kept as regressions. Pass 2 added the
specification, adversarial generators, property tests, Kani harnesses, and a
machine-checked TLA+ model; it found **two more defects**, one of them
introduced by a pass-1 fix.

Suite 255 -> 307 tests. `cargo test --workspace` green, `cargo clippy
--workspace --all-targets` clean, `cargo fmt --all --check` clean.

- The oracle: [SPEC.md](SPEC.md) -- every property, what checks it, and what is
  explicitly *not* covered.
- Findings: [CORRECTNESS_FINDINGS.md](CORRECTNESS_FINDINGS.md)
- Verification status, including what is written but unexecuted:
  [SPEC.md section 9](SPEC.md#9-verification-status)

## Headline

The corpus gates were green the whole time and caught **none** of this. Scores on
all three corpora are bit-identical before and after, because every row is ASCII,
none contains `improve`/`approved`, and none exercises `hybrid` mode. Meanwhile
the gateway returned 500 to any request containing a Japanese greeting, and a key
restricted to the `hard` tier could be served another tenant's cached `trivial`
answer.

Accuracy gating and correctness gating are different things. Only the second was
missing.

## 1. Baseline

- Workspace: 6 crates, ~15.9k LOC of Rust.
- Test distribution (255 tests): gateway 151, classifier 71, types 17, policy 9,
  evals 4, provider 3. Two `#[ignore]`d tests.
- All green. A green suite is not evidence of correctness; it only means the
  current assertions agree with the current code. The work below is about
  finding the places where both are wrong.

## 2. Method

For every candidate defect:

1. **Reproduce** it as a failing test that encodes the *documented* contract
   (`docs/LLD.md`, `docs/SECURITY.md`, `docs/OPERATIONS.md`) or an
   unambiguous invariant.
2. **Fix** the cause, not the symptom.
3. Keep the test, so the bug cannot return silently.
4. **Prove the test bites** by reverting the fix and confirming the failure. Six
   of the fifteen were confirmed this way; the rest fail on arithmetic that is
   demonstrable from the source.

No behaviour is changed on the strength of a hunch. If a contract is genuinely
ambiguous, the finding is recorded as a question rather than patched.

Where a fix touched a contract that had two copies of it, the copies were merged
into one function or one constant (`endpoint_url`, `FACTUAL_OPENER`,
`keeps_reasoning_pin`, `price_per_token`, `patch_field`, `tier_permitted`,
`StoredResponse`). A drifting second copy of a weaker rule is not a style
problem — it is how defect 12 survived a fix to one of its two causes.

Two tests written during this pass caught bugs in the fixes themselves (an
`owner` binding that swallowed its own valid case; a tier assertion inverted), and
one corpus gate caught an over-correction before it landed. Both are recorded in
the findings.

## 3. Risk-ordered audit targets

| Crate | Why it is risky |
|---|---|
| `miser-gateway/src/auth.rs` | rate-limit windows, budget accounting, key lifecycle, credential comparison |
| `miser-gateway/src/usage.rs` | cost/spend math, aggregation across retries and escalations |
| `miser-gateway/src/catalog.rs` | largest module; tier pins, capability filtering, model selection |
| `miser-gateway/src/semantic_cache.rs` | similarity scoring, eviction, cache-key derivation, tenancy |
| `miser-gateway/src/validate.rs` | config rejection before the socket binds |
| `miser-gateway/src/main.rs` | handlers, streaming, error mapping, concurrency |
| `miser-classifier/src/lib.rs` | tier decision, override parsing, Jev response handling |
| `miser-policy/` | effective-tier floor logic, quality scoring |
| `miser-provider/` | request rewriting, header filtering, upstream error mapping |
| `miser-types/` | serde contracts, defaults, round-trips |

## 4. Invariant classes under test

- **Authorization binds on every path.** A control enforced on one path and
  skipped on a fast path is a hole. Defect 3 is exactly this: the quota checks
  were hoisted above the cache and the tier gate was not.
- **No panic on attacker-chosen input.** Fixed-width byte slicing, index removal,
  and unwraps on request-derived data.
- **Conservation.** Budgets, rate limits and token accounting are neither
  double-counted nor lost across a retry, an escalation, a stream, or a failure —
  and a cap reserved up front is reconciled, not kept.
- **Fidelity.** Unknown fields survive a round trip; a cache key is a fingerprint
  of the request; a cache hit is attributed to the tier and model that produced it.
- **Fail-closed on money.** An unreadable price is not a price of zero; a missing
  usage frame falls back to the conservative estimate; a present-but-unparseable
  quota field is a client error, not "no restriction".
- **Monotonicity.** A pin that cannot do its tier's job is replaced even when
  hysteresis would keep it.
- **Serde fidelity.** Every field survives a round trip; defaults agree between
  serde and `Default`.

## 5. Deliverables

- `docs/CORRECTNESS_FINDINGS.md` — every defect, its root cause, and its fix.
- `docs/CORRECTNESS_PLAN.md` — this document.
- Regression test per defect (34 added).
- Green `cargo test --workspace`, clean `cargo clippy --workspace --all-targets`.

## 6. What remains

Done in pass 2: the specification, the seven decisions (all adopted; D4
implemented, along with the `hard_max` band that makes the ladder monotone), the
adversarial generators, 30 property tests, 12 Kani harnesses, a machine-checked
authorization model, and the tiered CI wiring.

Not yet done, in the order they should be picked up:

1. **Extract `miser-core`.** The routing, money and quota decisions sit inside a
   4,100-line `main.rs` tangled with axum and tokio. Kani cannot verify a
   function that borrows a socket, and `QuotaEnforcer` reads `SystemTime::now()`
   internally, so its window cannot be driven from a test. Until this lands the
   money path is property-tested but not provable -- and it is the
   highest-blast-radius code in the repository.
2. **Boot the quota state from the ledger (D1).** Every restart currently resets
   every monthly cap and every rate-limit window, so the effective ceiling is
   `cap x restarts`. Needs D1's `budget_charged_usd` ledger field.
3. **Finish D3, D5, D6, D7** -- cache default off, delete `max_cost_per_1m`,
   enforce capability floors, and write down the streaming-overrun bound.
4. **Execute the verification that is written but unrun.** Kani and
   `cargo-mutants` are committed and configured; both need x86_64 or a long local
   run. The first mutation baseline number is the most informative single
   measurement still outstanding.
5. **Validate the cascade's question against the live service.** It is the only
   model-authored prompt in the codebase with no live check, and it is the stage
   that escalates traffic.
