#!/usr/bin/env bash
# Create one GitHub issue per audit finding, and print a machine-readable
# index (number -> slug) so the commits and the closing pass can reference them.
#
# Kept in the repo rather than run ad hoc: the issue tracker is part of the audit
# trail, and the mapping from finding to issue should be reproducible.

set -euo pipefail
cd "$(dirname "$0")/.."

INDEX=".audit-issue-index"

for l in bug correctness security money routing formal-verification audit-pass-1 audit-pass-2; do
  gh label list --limit 200 --json name -q '.[].name' | grep -qx "$l" || \
    case "$l" in
      bug)                  gh label create bug --color d73a4a --description "Something is not working" >/dev/null ;;
      correctness)          gh label create correctness --color b60205 --description "A formally specified property is violated" >/dev/null ;;
      security)             gh label create security --color ee0701 --description "Authorization or data-isolation defect" >/dev/null ;;
      money)                gh label create money --color d4c5f9 --description "Cost, quota, or spend-accounting defect" >/dev/null ;;
      routing)              gh label create routing --color 1d76db --description "Tier selection or failover defect" >/dev/null ;;
      formal-verification)  gh label create formal-verification --color 5319e7 --description "Raised by a proof, model check, or property test" >/dev/null ;;
      audit-pass-1)         gh label create audit-pass-1 --color c2e0c6 --description "Found by the structured review pass" >/dev/null ;;
      audit-pass-2)         gh label create audit-pass-2 --color bfd4f2 --description "Found by the verification pass" >/dev/null ;;
    esac
done

: > "$INDEX"

issue() {
  # issue <slug> <labels> <title>  -- body on stdin
  local slug="$1" labels="$2" title="$3"
  local num
  num=$(gh issue create --title "$title" --label "$labels" --body-file - | sed 's#.*/##')
  printf '%s\t%s\n' "$slug" "$num" | tee -a "$INDEX"
}

# --------------------------------------------------------------- pass 1 ----

issue panics-on-non-ascii "bug,correctness,security,audit-pass-1" \
  '@route: prefix probe panics on any non-ASCII prompt (HTTP 500)' <<'EOF'
`override_tier` fell back to `first[..7]` — a fixed **byte** index with no
`is_char_boundary` guard — after `strip_prefix("@route:")` had already failed.

**Repro** (any `POST /v1/chat/completions`):

```json
{"model":"auto","messages":[{"role":"user","content":"日本語でこんにちは"}]}
```

```
panicked at crates/miser-classifier/src/lib.rs: end byte index 7 is not a
char boundary; it is inside '語' (bytes 6..9 of string)
```

`CatchPanicLayer` turns this into a bare 500 rather than the documented JSON
error with `x-miser-request-id`.

Input-dependent, which is why nobody hit it: `"@route:hard\n日本語で…"` does not
panic, and `"hi 😀 there"` happens to land on a boundary. `"日本語で…"`, `"abcd😀"`,
`"कऋग"` all do.

**Why the suite missed it.** `arb_text()` drew from a 77-word **ASCII** list, so
the input class was structurally unreachable. The 2,277 corpus rows contain zero
non-ASCII characters.

**Property.** [SPEC.md P7](../blob/main/docs/SPEC.md) — every parser is total.

**Fix.** `first.get(..7)?` (returns `None` rather than panicking), plus an
adversarial generator that places a multi-byte character at a chosen byte offset
so the class cannot recur unseen, and a Kani harness taking `any::<&str>()`.
EOF

issue semantic-cache-zero-panic "bug,correctness,audit-pass-1" \
  'Semantic cache panics and dies at max_entries = 0 — the documented way to disable it' <<'EOF'
```rust
if entries.len() >= self.max_entries { entries.remove(0); }   // 0 >= 0 on a fresh cache
```

`Vec::remove(0)` on an empty vector panics. LLD §6 documents `max_entries: 0` as
**the** switch that turns the cache off, so the documented configuration was a
live 500.

Worse, the panic happens under the lock, so it poisons the mutex: every later
`store` becomes a silent no-op and every `lookup` misses. The cache was dead for
the life of the process rather than merely erroring once.

`ResponseCache` never hit this — it is keyed, so a stale key simply never
matches, and its eviction is already guarded with `if let Some(..)`.

**Property.** [SPEC.md P9e](../blob/main/docs/SPEC.md) — no hit when
`max_entries = 0`.

**Fix.** `max_entries == 0` stores nothing and returns early, checked before the
eviction branch; the eviction is additionally non-empty guarded.
EOF

issue cache-bypasses-tier-allowlist "security,correctness,audit-pass-1" \
  'A response-cache hit bypassed the per-key tier allowlist (authorization bypass)' <<'EOF'
Handler order was:

```
auth → rate limit → budget → CACHE → classify → route → TIER GATE
```

The quota checks had been hoisted above the cache deliberately. `allowed_tiers`
had not — and a cache hit `return`s before classification, so the gate never ran.

**Repro**

1. Unrestricted key sends a request that classifies `Trivial`; body is cached.
2. Second key created with `allowed_tiers: ["hard"]`.
3. Second key sends the identical request.
4. **Actual:** `200 OK`, `x-miser-cache: hit-exact`, body from `test/trivial`.
   **Expected:** `403`.

Applies to responses cached by *any* tenant, and to the semantic cache as well.

**Why 151 tests missed it.** `tier_gating_returns_403_for_disallowed_tier` uses a
dead upstream (`127.0.0.1:9`), so the request 502s before anything is cached, and
each test gets a fresh `ResponseCache`. Nothing combined a warm cache with a
restricted key.

**Property.** [SPEC.md P8, P12a–d](../blob/main/docs/SPEC.md) — a cache hit is
subject to the same controls as a miss.

**Fix.** Both cache entry types carry the tier that produced them, and
`tier_permitted` is applied on the hit path. A disallowed tier is treated as a
*miss*, so a restricted key can still be served by a tier it may use.

**Also modelled.** `spec/AuthorizationOrdering.tla` — TLC proves the property
holds for the fixed handler and prints the pre-fix counterexample.
EOF

issue semantic-cache-crosses-tenants "security,correctness,audit-pass-1" \
  'Semantic cache hits cross API keys — one tenant served another tenant'"'"'s answer' <<'EOF'
`AppState` holds one process-global semantic cache and entries carried no tenant.
Similarity is a cosine over **message text only**, so two tenants asking *nearly*
the same question matched while their system prompts — the part that says whose
data the answer is about — were outvoted by the shared question.

**Repro**

- A: system `You are AcmeCorp billing. Customer 4471 owes $12,400.` + a ~70-word question.
- B: system `Be brief.` + the same question.
- **Actual:** `x-miser-cache: hit-semantic`, similarity 0.96, B receives A's body
  and never reaches the provider. B's ledger records zero tokens.

Fires in the *no-judge* configuration, which is the shipped one: the default
`similarity_threshold` of 0.92 is cleared by a shared question of only ~40 words.
The similarity score is therefore not a security boundary, and must not be
treated as one.

`semantic_enabled = true` in the shipped `config/miser.toml`, so this is live.

**Note.** The *exact* cache does not have this problem and needs no partition:
its key hashes the whole request body, so a hit there is a byte-identical
request and the shared answer is the right answer to the question asked.

**Fix.** Entries carry a `tenant` (the key id; one shared partition in
open-access setup mode) and `lookup` filters on it.
EOF

issue content-part-other-destroys-part "bug,correctness,audit-pass-1" \
  'Unrecognised content parts were rewritten to {"type":"Other"}' <<'EOF'
```rust
#[serde(other)] Other,   // internally tagged *unit* variant: nowhere to keep the original
```

Both the `type` tag and every field on the part were destroyed on
re-serialization. Any part outside `{text, image_url, input_audio, refusal}` —
`file`, `input_file`, `document`, `video_url`, Anthropic `thinking`/`tool_use`,
Gemini `inline_data` — reached the provider as `{"type":"Other"}` while the
client received a cheerful 200.

LLD §2 states the intent: `extra` "prevents the gateway from dropping
provider-specific options". `ChatMessage.extra` and `ImageUrl.extra` both do this
correctly; `ContentPart` was the one place that dropped instead.

The old test asserted only that *decoding* did not fail, never that the decoded
value re-serialised.

**Property.** [SPEC.md P15a/b](../blob/main/docs/SPEC.md) — round-trip fidelity.

**Fix.** See the companion issue on integer tags; the deserialize path is now
hand-written so "unrecognised means verbatim" is structural.
EOF

issue streaming-usage-not-read "money,correctness,audit-pass-1" \
  'Streaming responses were accounted from the max_tokens estimate, not the provider'"'"'s real usage' <<'EOF'
The SSE body was relayed unparsed, so the provider's final `usage` frame was never
read. The ledger recorded `prompt_tokens: 0` and the `max_tokens` estimate as
completion tokens, and the same fiction drove the budget charge.

**Repro.** Mock SSE carrying `usage:{"prompt_tokens":1200,"completion_tokens":37}`,
tier `max_tokens = 4096`, `price_per_1k_usd = 0.002`:

| | prompt | completion | cost |
|---|---:|---:|---:|
| actual | 0 | 4096 | 0.008192 |
| expected | 1200 | 37 | 0.002474 |

In the shipped config `hard` is `max_tokens = 8192`, so a 200-token answer was
reported as 8,192 — **41×** — and the monthly cap was consumed at the same rate,
`402`-ing a paying key for work it never did. Every rollup (`by_model`, `by_key`,
`by_client`, `by_day`) inherited it.

Violates LLD §2: "the reported figure in `/admin/usage/*` uses the provider's
real `usage` block".

**Fix.** `MeteredStream` keeps a bounded 8 KiB tail and finalises accounting when
the stream ends. The budget is still *reserved* on the estimate up front so the
cap holds before the answer exists; `QuotaEnforcer::adjust_spend` then reconciles
the reservation to the real cost, against the window it was reserved in (so a
stream crossing midnight UTC does not reconcile into the wrong month) and floored
at zero. With no `usage` frame the estimate still stands in — recording a
confident zero would under-report spend.
EOF

issue semantic-cache-ignores-generation-params "bug,correctness,audit-pass-1" \
  'Semantic cache identity ignored max_tokens, temperature, top_p and stop' <<'EOF'
`request_text_for_embedding` hashed only `messages[].content`, so every
generation parameter was invisible to the cache. A request capped at 8 tokens was
served the cached body of an otherwise identical request allowed 4096 —
including the first request's `usage` and `model` fields.

The in-code claim that the no-judge fallback "mirrors the exact-match safety bar"
was false: `request_hash` keeps all of these in its key.

**Property.** [SPEC.md P9a/b](../blob/main/docs/SPEC.md) — a cache key is a
fingerprint of the request.

**Fix.** The parameters are appended to the embedding text (the equivalence
judge is handed that same text, so it should see what it is comparing).
EOF

issue budget-window-not-calendar "money,correctness,audit-pass-1" \
  'monthly_budget_usd used a 30.44-day window, so a cap could be spent 2–3× inside one calendar month' <<'EOF'
```rust
(secs / 2_629_800) as u32   // 30 days 10.5 hours
```

An average month. Each window started 10.5 h later than the last, so boundaries
fell **inside** calendar months.

- window 672 = `[2026-01-01T00:00:00Z, 2026-01-31T10:30:00Z)`
- window 673 starts at 10:30 on Jan 31

So 21.5 h of calendar January 2026 belonged to the window that also held most of
March. 11 of 12 months in 2026 contain a boundary; **January 2027 contains two**.

A key with a $100 cap could spend $100 inside one calendar month, then another
$100 after the next boundary, before the month turned over — up to $300 in
January 2027. The old window's total was discarded, not carried forward.

**Why tests missed it.** `current_month()` read `SystemTime::now()` directly with
no injectable clock, so no test could reach the transition.

**Property.** [SPEC.md P2a–d](../blob/main/docs/SPEC.md), including P2c
(`window(t) ≤ window(t + 31 days)`), which the old divisor violates by skipping
February in a non-leap year.

**Fix.** `calendar_month_index` via Hinnant's `civil_from_days`, so the index
changes exactly at midnight UTC on the 1st and nowhere else. Property-tested
against an independently written reference calendar.
EOF

issue create-key-drops-quota-fields "security,money,audit-pass-1" \
  'POST /admin/keys silently dropped or truncated every quota field' <<'EOF'
The handler read the body with `Value::as_*`, where a wrong-typed value yields
`None` — which for a quota field *is* "no restriction". The sibling `PATCH`
handler already rejected exactly this class via `patch_field` (its own comment
records the `as u32` truncation bug); only `create_key` was left guessing in the
operator's favour.

| request body | stored | effect |
|---|---|---|
| `{"rate_limit_rpm":"60"}` | `null` | **no rate limit** |
| `{"monthly_budget_usd":"10.00"}` | `null` | **no spend cap** |
| `{"allowed_tiers":"hard"}` | `[]` | **every tier allowed** |
| `{"expires_at":"1767225600"}` | `null` | **never expires** |
| `{"rate_limit_rpm":4294967296}` | `0` | **429 on every request** |

`"60"` is what a shell-quoted curl, or any client that stringifies numerics,
sends. All six returned `200` with a working key.

**Fix.** `create_key` now routes through the same `patch_field` helper, plus
rejection of non-finite/negative money and rpm and of an empty `owner`. Covered
by a directed test and a generated one over arbitrary JSON types.
EOF

issue improve-mints-reasoning-task "routing,correctness,audit-pass-1" \
  '"improve" minted a Reasoning task, an unbounded floor to the most expensive tier' <<'EOF'
`coding_or_reasoning` used word-anchored `has_word` for all 11 Coding keywords and
raw `contains` for the three Reasoning ones. `has_word` exists precisely because
`contains("code")` fires on "unicode"; the Reasoning side was missed.

**Repro.** `"improve the test coverage of the http client"` →
`tier=Standard`, `task=Some(Reasoning)`, so `effective_tier=Reasoning` →
`z-ai/glm-5.3` at `max_tokens=8192`. Also `"improve the parser"` and
`"the improvement ticket is approved, ship it"`.

Sharpest because `miser-policy` turns a Reasoning task into an **unbounded**
`max(tier, Reasoning)`, unlike the Coding false positive which is bounded by a
guard. `"please improve the retry logic"` is unaffected — `has_word("retry")`
hits first, so the bug fires precisely when no Coding keyword is present.

**Fix.** `has_word` for the Reasoning list too, with a test proving genuine
reasoning prompts are still detected — the fix must not trade an over-route for
an under-route.
EOF

issue binary-question-forced-trivial "routing,correctness,audit-pass-1" \
  'Any yes/no question about real work was forced to Trivial at 0.95 confidence' <<'EOF'
Two independent causes, which is why fixing one was not enough.

**1.** The short-definitional override's binary row accepted *any* yes/no
question up to 60 characters, satisfying none of the three conditions that
function documents ("a definitional opener, a brevity constraint, a short request
in total"). It set `scores[0] = 100` and zeroed everything else, so the tier was
Trivial at the 0.95 confidence cap — above both the 0.65 tier-floor threshold
and the 0.70 verification threshold, so neither the policy floor nor the cascade
could recover it.

**Repro.** `"true or false: delete every row in prod and rebuild the index?"` →
`Trivial`, confidence 0.95, `effective_tier=Trivial` → `mistralai/mistral-nemo` at
`max_tokens=256`.

**2.** With that fixed, the `trivial` tier's *own* binary pattern (worth 5) still
decided it alone: when it is the only match it wins 5-to-0 against a field of
zeros and picks the tier by itself.

**Fix.** The discriminator is not the binary framing but whether the question asks
for a *fact* or requests *work*: a leading copula or quantifier
(`is|are|was|were|does|did|has|have|any|all|every`) keeps it Trivial, an
imperative does not. `can/could/will/would` are admitted only in the third person,
because that separates "can a primary key be null?" from "can you add retries?";
`should` is excluded outright. That last distinction needs a negative lookahead
and `regex` has none, so it is a small function rather than a pattern.

Both patterns build from one shared `FACTUAL_OPENER` — a drifting second copy of
a weaker pattern that decides the tier alone is how this survived a fix to one of
its two causes.

**Note.** The first attempt required a brevity marker instead and cost 0.5 points
on `classifier_cases.jsonl`. The corpus pins
`"just answer yes or no: is Python interpreted?"` at Trivial, and the corpus was
right to.
EOF

issue hysteresis-keeps-incapable-pin "routing,correctness,audit-pass-1" \
  'Hysteresis could not retire a Reasoning pin that could not reason' <<'EOF'
`prefer_reasoning_pin` was enforced in `pin_choice` and in the candidate loop,
but not in the "keep the incumbent" arm, which compared price only.

**Repro.** Defaults; incumbent `seeded/non-reasoning` @ $1.00/M; `pool/thinker-a`
@ $1.30 (reasoning), `pool/thinker-b` @ $1.35. `pin_choice` → `pool/thinker-a`;
`1.30 <= 1.00 * 0.75` is false, so hysteresis kept the incapable incumbent.

**Unrecoverable by refresh.** Hysteresis only ever migrates to a *cheaper* model,
and a capable one costs more, so the pin could never move up. The only escape was
a three-failure failover streak.

**Fix.** A shared `keeps_reasoning_pin` predicate used by all three sites, with a
control test showing the fix did not disable pin stability on tiers with no
capability requirement.
EOF

issue stale-candidates-after-empty-pool "routing,correctness,audit-pass-1" \
  'A catalog refresh kept a stale pin'"'"'s stale candidate list' <<'EOF'
A tier whose filtered pool is empty keeps its previous pin — documented, and
sensible. But the preserved `TierPin` carried a `candidates` list validated against
the catalog the refresh had just superseded, and `report_failure` walks
`candidates[position+1]` unconditionally. So a 5xx streak promoted a successor the
refresh had proven is not a viable candidate.

And because a 404 from a delisted model is classed as a *client* error,
`report_failure` was never called again, so the tier stayed wedged on the stale
pin with no route out.

**Fix.** Retain only candidates still present in the new catalog. Keeping the pin
stays deliberate; the stale candidates do not.
EOF

issue escalation-transport-error-invisible "routing,correctness,audit-pass-1" \
  'A quality-gate escalation that never completed was invisible to failover' <<'EOF'
```rust
if let Ok(escalated_upstream) = state.provider.forward(..).await { ... }   // no else
```

The first call's transport error *was* reported, and an escalation returning 5xx
*was* reported — but a connection that never completed had no arm at all.

**Repro.** Mock that answers call 0 normally and hangs up on call 1, with the
quality gate forced to escalate. Result: 2 upstream calls, client gets `200`, and
`miser_upstream_errors_total` stays at `0`.

So a model that reliably dies on escalated requests could never accumulate the
three failures `failover_threshold` needs — the opposite of the intent recorded
at the call site, which is that the escalated-*to* model is the one to retire.

**Fix.** The missing `else`. The test uses a raw `TcpListener`, because the
failure cannot be produced through an HTTP handler.
EOF

issue unreadable-price-became-free "money,correctness,audit-pass-1" \
  'An unreadable catalog price became $0.00/M, making a paid model the cheapest' <<'EOF'
`unpriceable` only caught a **string** that parsed to a non-finite number. A JSON
`null`, an object, a bool, an array, or `"see pricing page"` all fell through to
`0.0` — and with `allow_free = true` that made a paid model the *cheapest*
candidate in its band, and the first failover target.

OpenRouter currently returns prices as strings, so this needs a provider payload
change to trigger; with the default `allow_free = false` the damage is that the
model is excluded rather than mis-ranked.

**Fix.** One `price_per_token` extractor shared by both questions, so "present
but unreadable" means the same thing whatever JSON type carried it. A JSON
*number* is now honoured rather than dropped or zeroed.
EOF

issue escalate-on-failure-two-defaults "bug,money,audit-pass-1" \
  'QualityConfig::escalate_on_failure had two different defaults' <<'EOF'
serde `#[serde(default)]` → `false`; `impl Default` → `true`.

Unobservable while `enabled` is false, so the only place it showed was a config
naming `[quality]` with `enabled = true` and omitting the key: the documented
quality-gate escalation never ran, while dropping the whole table gave `true`.
**Naming a table changed the meaning of a key inside it.**

The existing test recorded the divergence in a comment and then asserted the
serde value as though it were correct.

**Fix.** Aligned on `false` — a second, separately billed upstream call should be
something an operator asks for, not something that appears because a heading was
typed.
EOF

issue nan-band-bounds-accepted "routing,correctness,audit-pass-1" \
  'validate_config could not reject NaN band bounds; the tier split collapsed silently' <<'EOF'
Every `<=`/`>=` is false against NaN, and TOML 1.0 has a `nan` float literal that
`toml` 0.9 accepts. So `standard_max = nan` passed all four order checks and the
gateway started normally.

Then `band_tier`'s `<= NaN` tests all failed, so every model in the affected price
slice fell through to the next band **up** — a silent, whole-catalog re-routing
to more expensive models.

The message the function emits asserts a strict ordering, so the check has to test
finiteness too. `switch_saving_ratio` was already correct: `Range::contains` does
reject NaN there.

**Fix.** Finiteness checked alongside the ordering, with tests asserting both the
rejection and the collapse it prevents.
EOF

issue miser-catalog-file-not-an-override "bug,correctness,audit-pass-1" \
  'MISER_CATALOG_FILE was a fallback, not the override the docs claim' <<'EOF'
`miser-types:595` documents the path as "overridable via `MISER_CATALOG_FILE`",
but `snapshot_path` checked `routing.cache_path` first and only fell back to the
environment variable. The shipped `config/miser.toml` sets
`cache_path = "catalog/models.json"`, so the documented override was **inert in
the shipped deployment** — and therefore untestable by anyone following the docs.

**Fix.** Environment variable consulted first; an empty value falls through.
EOF

issue cache-hits-misattributed "bug,money,audit-pass-1" \
  'Cache hits were attributed to the requested model and to tier "-"' <<'EOF'
`usage.rs` defines `model` as "the model that actually served the request", but
both cache paths passed `&request.model` — still the pre-routing client value,
e.g. `"auto"` — and `"-"` for the tier.

`by_model` grew a phantom bucket for a model that does not exist, splitting one
model's rollup and pricing the cache-served half at $0; `by_tier` grew a `"-"` bucket
per hit. `usage.rs`'s own test asserts the correct contract but hand-builds its
records, so it could not see the producer.

**Fix.** Both cache entry types now record the tier, and the serving model is
attributed to the route that answered.
EOF

# --------------------------------------------------------------- pass 2 ----
# These were found by the verification layer, not by review. Each names the
# tool that found it.

issue price-scaling-overflows-to-neg-inf "money,correctness,formal-verification,audit-pass-2" \
  'Price finiteness was checked before the per-million scaling, so a finite price could still become -inf' <<'EOF'
Found by `properties::an_accepted_price_is_finite`, and **introduced by the fix
for the issue above** — which is the point of writing it down.

The finiteness check ran on the per-token value, but the ×10⁶ scaling to
per-million can itself overflow:

```
f64::MIN * 1_000_000.0  ==  -inf
```

`f64::MIN` is perfectly finite, so the check passed. Then every band comparison
is false while `input_price_per_m <= 0.0` is true, so the model reads as **free**:
banded Trivial, and pinned as the cheapest thing in the catalog.

**Fix.** `price_per_million` performs the scaling and checks finiteness on the
result; both the rejection and the honouring of a JSON number read from it.
EOF

issue integer-content-tag-reads-variant-index "bug,correctness,formal-verification,audit-pass-2" \
  'A JSON array content part with an integer first element was decoded as a text part' <<'EOF'
Found by `parsers::an_arbitrary_content_part_round_trips_verbatim`, on its first run.

With `#[serde(untagged)]` over an internally tagged inner enum, serde *also*
accepts its **sequence** form, where element 0 is the tag — and there an integer
tag selects a variant by **position**. So:

```json
{"content": [[0, ""]]}
```

decoded to `Text { text: "" }` and was forwarded as
`{"type":"text","text":""}`: a malformed part silently replaced by a different,
well-formed one. `[1, "hi"]` correctly failed, because `ImageUrl`'s payload is
not a string — which is why it looked unreachable.

Same defect class as the `{"type":"Other"}` rewrite, found by a generated input
rather than by reading.

**Fix.** `Deserialize` is hand-written so the rule is structural: a part is
`Known` only if it is an object whose `type` is one of the four known strings, and
`Other` otherwise, unchanged. Serialization stays `untagged` so `Known` keeps its
tag and `Other` re-emits verbatim — an intermediate version dropped that and
re-introduced the corruption as `{"Other": ...}`, which the same round-trip test
caught.
EOF

issue band-ladder-inverted "routing,correctness,formal-verification,audit-pass-2" \
  'The Reasoning price band sat *below* Hard, inverting the top of the tier ladder' <<'EOF'
Found by `properties::band_assignment_is_monotone_in_price`, and decided as **D4**.

`band_tier` tested `reasoning_max` *before* the `Hard` fallthrough, so `Hard` was
"everything above `reasoning_max`" — above Reasoning in price, and therefore
**below** it in tier strength. Concretely: $1.40 banded Reasoning while $2.00
banded Hard.

```
band_assignment_is_monotone_in_price: cheaper price 1.4 gave a stronger tier
(Reasoning) than 693.7 (Hard)
```

`prefer_reasoning_pin` existed only to patch the consequence, and patched only the
Reasoning tier.

**Fix.** A `hard_max` band added, so the ladder is
`trivial ≤ simple ≤ standard ≤ hard ≤ reasoning` and the tier assignment is
monotone in price. Anything above the top band resolves to the strongest tier
rather than wrapping down to a weaker one.

Nine existing catalog tests encoded the old inverted ladder and were updated to
the corrected bands, preserving each one's intent.
EOF

# ------------------------------------------------------------- tracking ----

issue quota-state-resets-on-restart "money,correctness" \
  'Quota state is in-memory only: every restart resets every monthly cap and rate-limit window' <<'EOF'
**Recorded, not fixed.** `QuotaEnforcer::new` starts two empty maps; nothing is
loaded from disk at boot, unlike the key store (persisted atomically) and the
usage ledger (JSONL).

So every deploy, crash or `systemctl restart` hands every key its **entire
remaining monthly budget** and a free `rpm` burst — the effective ceiling is
`cap × restarts`.

**Repro**

```rust
let before = QuotaEnforcer::new();
before.record_spend("key_x", 10.0);
assert!(!before.check_budget("key_x", 10.0));   // cap exhausted
let after = QuotaEnforcer::new();                // exactly what main() builds
assert!(after.check_budget("key_x", 10.0));      // ACTUAL: full cap restored
```

The per-key data to reconstruct it is already written to the ledger on every
request — but the ledger's `cost_usd` is the real-usage figure while the budget
charges the `max_tokens` estimate, so a reconstruction must use the same formula,
not the recorded cost. That is why decision **D1** adds a `budget_charged_usd`
field.

**Blocked on.** The `miser-core` extraction, because `QuotaEnforcer` currently
reads `SystemTime::now()` internally and its window cannot be driven from a test.
EOF

issue extract-miser-core "correctness" \
  'Extract a pure miser-core so the money and routing decisions are provable' <<'EOF'
The prerequisite for proving anything that matters.

The interesting decisions live inside a 4,100-line `main.rs` with axum handlers,
`tokio` and `reqwest` tangled through them. Consequences:

- **Kani cannot see them.** A function that borrows a socket is not analysable.
- **`QuotaEnforcer` is untestable at the boundary.** It reads
  `SystemTime::now()` internally, so the calendar-window property could not be
  tested directly until a reference calendar was written alongside it.
- **The authorization ordering is a real invariant with no single owner.** It is
  spread across ~400 lines of handler and is now modelled in TLA+ rather than
  enforced by a type.

**Shape.** A `miser-core` crate with no tokio, no axum, no reqwest, no fs: data
in, decision out. `band_tier`, the price extractor, `QuotaEnforcer`, the capability
floor and the tier-floor logic move there; the gateway becomes a thin adapter.

**Sequencing.** Phased, decision logic first, proving each slice before moving
on — rather than one large refactor.
EOF

issue cascade-question-unverified "correctness,formal-verification" \
  'The verification cascade'"'"'s question has no live validation of any kind' <<'EOF'
`answers.tier_is_right.noul` is the only piece of model-authored text in the
codebase with zero live validation: `tests/cascade.rs` feeds a mock the number,
and `live_jev_contract` — the one `#[ignore]`d test that would exercise the real
service — does not cover the cascade's question at all.

This is the least verified and most consequential stage in the classifier: it can
**escalate traffic**. The `probabilities` fallback is likewise validated only
against hand-written mocks.

**Needs.** `JEV_API_KEY` in a scheduled CI run. The workflow now has a
`live-contract` job for exactly this, gated on the secret so a fork still gets a
green run.
EOF

issue decision-semantic-cache "correctness" \
  'Decide the semantic cache: finish it or turn it off' <<'EOF'
Even after partitioning by tenant and parameterising by generation settings, the
cache decides a hit from a bag-of-words cosine whose threshold is a tuning choice,
not a guarantee.

LLD §6 already records the original author's conclusion: coding prompts share
vocabulary, causing false-positive hits "at any threshold below 1.0", with the
recommendation to use real sentence embeddings or exact-match only.
`semantic_enabled = true` in the shipped config contradicts "disabled".

**Options.** Either finish it (real embeddings, documented threshold rationale) or
default it off and say so. Keeping it enabled while the evidence says it produces
false positives is the current, and worst, option.
EOF

echo
echo "index written to $INDEX"
cat "$INDEX"
