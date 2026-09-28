# Correctness findings

Fifteen defects, each reproduced as a failing test before the fix and kept
afterwards. Suite went 255 → 289 tests; `cargo test --workspace` green,
`cargo clippy --workspace --all-targets` clean, `cargo fmt --all --check` clean.

Method and scope: [CORRECTNESS_PLAN.md](CORRECTNESS_PLAN.md).

---

## The finding that matters most

**The corpora are blind to every defect below.**

`evals/*.jsonl` scores are *bit-identical* before and after this pass:

| Corpus | Exact | Under | Over | Change |
|---|---:|---:|---:|---|
| `cases.jsonl` | 1.0000 | 0.0000 | 0.0000 | none |
| `classifier_cases.jsonl` | 0.9138 | 0.0172 | 0.0690 | none |
| `classifier_cases_large.jsonl` | 0.9514 | 0.0200 | 0.0286 | none |

Every corpus gate was green while the gateway returned 500 to any request
containing a Japanese greeting, and while a key restricted to the `hard` tier
was served another tenant's cached `trivial` answer.

All 2,277 corpus rows are pure ASCII, none contains `improve`/`approved`, and
no row exercises `mode = "hybrid"`. The gates measure *tier choice on clean
English prose*; the defects below are crashes, authorization holes, and
accounting errors that no tier label would ever catch. Accuracy gating and
correctness gating are not the same thing, and only the second one was missing.

---

## Crashes

### 1. `@route:` probe panicked on any non-ASCII prompt — `classifier/src/lib.rs`

`first[..7]` sliced at a fixed byte index with no `is_char_boundary` guard, in
the arm that only runs when `strip_prefix("@route:")` has *already* failed.

- Input: `{"model":"auto","messages":[{"role":"user","content":"日本語でこんにちは"}]}`
- Actual: `panicked: end byte index 7 is not a char boundary; it is inside '語'`.
  `CatchPanicLayer` turns this into a bare 500, not the documented JSON error.
- Unlucky inputs only: `"日本語でこんにちは"` panics, `"@route:hard\n日本語で…"`
  does not, and `"hi 😀 there"` happens to land on a boundary.
- Fixed with `first.get(..7)?`, which returns `None` instead of panicking.
- Tests: `a_non_ascii_prompt_never_panics_the_route_prefix_probe`,
  `a_route_directive_still_wins_with_a_non_ascii_body`.

### 2. `max_entries = 0` panicked and poisoned the semantic cache — `gateway/src/semantic_cache.rs`

```rust
if entries.len() >= self.max_entries { entries.remove(0); }   // 0 >= 0 on a fresh cache
```

`Vec::remove(0)` on an empty vector panics. LLD §6 documents `max_entries: 0` as
*the* way to switch the cache off, so the documented configuration was a live
500 — and because the panic happens under the lock, every later `store` became a
silent no-op and every `lookup` missed. The cache was dead for the life of the
process.

`ResponseCache` never hit this: it is keyed, so at capacity 0 an entry is simply
unreachable, and its eviction is already guarded with `if let Some(..)`.

- Fixed: `max_entries == 0` stores nothing (and is checked before the eviction
  branch, which is now also non-empty guarded).
- Tests: `a_zero_capacity_cache_is_disabled_rather_than_a_one_entry_cache`,
  `a_one_capacity_cache_keeps_exactly_the_newest_entry`.

---

## Authorization

### 3. A cache hit bypassed the per-key tier allowlist — `gateway/src/main.rs`

Handler order was auth → rate limit → budget → **cache** → classify → route →
**tier gate**. The quota checks had been hoisted above the cache deliberately;
`allowed_tiers` was left below it, and a cache hit returns before
classification — so the gate never ran.

- Warm the cache with an unrestricted key, restrict a second key to
  `allowed_tiers: ["hard"]`, resend the identical request.
- Actual: `200 OK`, `x-miser-cache: hit-exact`, body from `test/trivial`.
  Expected: `403`.
- Applies to responses cached by *any* tenant, and the semantic cache too.
- Fixed: both cache entry types now carry the tier that produced them, and
  `tier_permitted` is applied on the hit path. A disallowed tier is treated as a
  *miss* rather than a 403, so a restricted key can still be served by a tier it
  is allowed to use. The three enforcement points share one predicate
  (`tier_permitted`) precisely so a fourth path cannot forget it.
- Tests: `a_cached_entry_is_not_served_to_a_key_barred_from_its_tier`,
  `a_cached_entry_is_still_served_to_a_key_with_no_allowlist`.

### 4. Semantic cache hits crossed API keys — `gateway/src/semantic_cache.rs`

`AppState` holds one process-global semantic cache and entries carried no tenant.
Similarity is a cosine over message text, so two tenants asking *nearly* the
same question matched each other while their system prompts — the part that says
whose data the answer is about — were outvoted by the shared question.

- Request A: system `You are AcmeCorp billing. Customer 4471 owes $12,400.` plus
  a ~70-word question. Request B: system `Be brief.` plus the same question.
- Actual: `x-miser-cache: hit-semantic`, similarity 0.96, B receives A's body
  and never reaches the provider. B's ledger records zero tokens.
- Fires in the *no-judge* configuration, which is the shipped one: the default
  `similarity_threshold` of 0.92 is cleared by a shared question of only ~40
  words. This is why the similarity score must not be treated as a security
  boundary.
- `semantic_enabled = true` in the shipped `config/miser.toml`, so live.
- The **exact** cache does not have this problem and needs no partition: its key
  hashes the whole request body, so a hit there is a byte-identical request and
  the shared answer is the right answer to the question actually asked.
- Fixed: entries carry a `tenant` (the key id; one shared partition in
  open-access setup mode) and `lookup` filters on it.
- Tests: `a_semantic_hit_never_crosses_api_keys` (asserts the cosine clears the
  threshold first, so the tenant boundary is the only thing doing the work),
  `a_semantic_hit_still_fires_within_one_api_key`, `a_lookup_never_crosses_tenants`.

---

## Request corruption

### 5. Unrecognised content parts were rewritten to `{"type":"Other"}` — `miser-types`

```rust
#[serde(other)] Other,   // internally tagged *unit* variant: nowhere to put the original
```

Both the `type` tag and every field on the part were destroyed on
re-serialization. Any part outside `{text, image_url, input_audio, refusal}` —
`file`, `input_file`, `document`, `video_url`, Anthropic `thinking`/`tool_use`,
Gemini `inline_data` — reached the provider as `{"type":"Other"}`, while the
client received a cheerful 200. LLD §2 states the intent: `extra` "prevents the
gateway from dropping provider-specific options". `ChatMessage.extra` and
`ImageUrl.extra` both do this correctly; `ContentPart` was the one place that
dropped instead. The old test asserted only that *decoding* did not fail.

- Fixed: `ContentPart` is now `Known(KnownContentPart) | Other(Value)` under
  `#[serde(untagged)]`, so known parts keep their `type` tag and unknown parts
  are carried verbatim.
- Tests: `an_unmodelled_content_part_survives_a_round_trip_verbatim`,
  `a_known_content_part_keeps_its_type_tag_when_re_encoded`,
  `text_extraction_skips_unmodelled_parts_without_panicking`.

### 6. Streaming accounting reported the `max_tokens` estimate — `gateway/src/main.rs`

The SSE body was relayed unparsed, so the provider's final `usage` frame was
never read. The ledger recorded `prompt_tokens: 0` and
`estimated_call_tokens(&request)` as completion tokens, and the same fiction
drove the budget charge.

- Mock SSE with `usage:{"prompt_tokens":1200,"completion_tokens":37}`, tier
  `max_tokens = 4096`, `price_per_1k_usd = 0.002`.
- Actual: `prompt_tokens: 0, completion_tokens: 4096, cost_usd: 0.008192`.
  Expected: `1200, 37, 0.002474`. In the shipped config `hard` is
  `max_tokens = 8192`, so a 200-token answer was reported as 8,192 — 41x — and
  the monthly cap was consumed at the same rate, 402-ing a paying key for work
  it never did.
- Violates LLD §2: "the reported figure in `/admin/usage/*` uses the provider's
  real `usage` block".
- Fixed with `MeteredStream`, which keeps a bounded 8 KiB tail of the relayed
  body and finalises accounting when the stream ends (`usage_from_sse` scans
  backwards for the usage frame, `None` meaning "no frame"). The budget is still
  *reserved* up front on the estimate, so the cap holds before the answer exists;
  `QuotaEnforcer::adjust_spend` then reconciles the reservation to the real cost,
  against the window it was reserved in (so a stream crossing midnight UTC does
  not reconcile into the wrong month) and floored at zero (a reconciliation must
  not manufacture credit).
- With no `usage` frame the estimate still stands in — recording a confident zero
  would under-report spend, the one direction that must not happen.
- Tests: `a_streamed_response_is_accounted_from_the_providers_real_usage`,
  `a_streaming_reservation_is_reconciled_to_the_real_cost`,
  `a_stream_without_a_usage_frame_falls_back_to_the_estimate`.

### 7. Generation parameters were invisible to the semantic cache — `gateway/src/semantic_cache.rs`

`request_text_for_embedding` hashed only `messages[].content`, so `max_tokens`,
`temperature`, `top_p` and `stop` were absent from the cache identity. A request
capped at 8 tokens was served the cached body of an otherwise identical request
allowed 4096, including the first request's `usage` and `model` fields. The
in-code claim that the no-judge fallback "mirrors the exact-match safety bar" was
false: `request_hash` keeps all of these in its key.

- Fixed: the parameters are appended to the embedding text (the judge is handed
  that same text, so it should see what it is comparing).
- Test: `generation_parameters_are_part_of_the_embedding_text`, which also
  asserts the similarity between the two variants actually *drops* — a token the
  shared prose outvotes would not be enough.

---

## Money and quotas

### 8. `monthly_budget_usd` was not a calendar month — `gateway/src/auth.rs`

```rust
(secs / 2_629_800) as u32   // 30 days 10.5 hours
```

An average month. Each window started 10.5 h later than the last, so boundaries
fell *inside* calendar months.

- Window 672 = `[2026-01-01T00:00:00Z, 2026-01-31T10:30:00Z)`; window 673 starts
  at 10:30 on Jan 31. So 21.5 h of calendar January belonged to the window that
  also held most of March. 11 of 12 months in 2026 contain a boundary; **January
  2027 contains two**.
- A key with a $100 cap could therefore spend $100 inside one calendar month, then
  another $100 after the next boundary, before the month turned over — up to
  $300 in January 2027. The old window's total was discarded, not carried.
- Fixed with `calendar_month_index` (Hinnant's `civil_from_days`), so the index
  changes exactly at midnight UTC on the 1st and nowhere else.
- Tests: `the_budget_window_turns_over_exactly_at_the_start_of_a_calendar_month`,
  and `a_monthly_cap_cannot_be_respent_within_one_calendar_month` — which
  enumerates every hour of January 2026 and asserts the old index split it,
  so the test fails if the premise stops being true.

### 9. `POST /admin/keys` silently dropped or truncated every quota field — `gateway/src/main.rs`

Read with `Value::as_*`, where a wrong-typed value yields `None` — which for a
quota field *is* "no restriction". The sibling `PATCH` handler already rejected
exactly this class via `patch_field` (its own comment records the `as u32`
truncation bug); only `create_key` was left guessing in the operator's favour.

| request body | stored | effect |
|---|---|---|
| `{"rate_limit_rpm":"60"}` | `null` | **no rate limit** |
| `{"monthly_budget_usd":"10.00"}` | `null` | **no spend cap** |
| `{"allowed_tiers":"hard"}` | `[]` | **every tier allowed** |
| `{"expires_at":"1767225600"}` | `null` | **never expires** |
| `{"rate_limit_rpm":4294967296}` | `0` | **429 on every request** |

`"60"` is what a shell-quoted curl, or any client that stringifies numerics,
sends. All six returned `200` with a working key.

- Fixed by routing `create_key` through the same `patch_field` helper, plus
  rejection of non-finite/negative money and rpm, and of an empty `owner`.
- Tests: `create_key_rejects_unusable_quota_fields_instead_of_dropping_them`,
  `create_key_keeps_well_formed_quota_fields`.

### 10. Cache hits were attributed to the requested model and tier `"-"` — `gateway/src/main.rs`

`usage.rs` defines `model` as "the model that actually served the request", but
both cache paths passed `&request.model` — still the pre-routing client value,
e.g. `"auto"` — and `"-"` for the tier. `by_model` grew a phantom bucket for a
model that does not exist, splitting one model's rollup and pricing the
cache-served half at $0; `by_tier` grew a `"-"` bucket per hit.
`usage.rs`'s own test asserts the correct contract, but hand-builds its records,
so it could not see the producer. Fixed by storing and reporting the real tier
and model.

---

## Under-routing

### 11. `improve` minted a `Reasoning` task, an unbounded floor to the top tier — `classifier/src/lib.rs`

`coding_or_reasoning` used word-anchored `has_word` for all 11 Coding keywords
and raw `contains` for the three Reasoning ones. `has_word` exists precisely
because `contains("code")` fires on "unicode"; the Reasoning side was missed.

- Input: `"improve the test coverage of the http client"`.
- Actual: `tier=Standard`, `task=Some(Reasoning)`, so `effective_tier=Reasoning`
  → `z-ai/glm-5.3` at `max_tokens=8192`. Expected: `Standard`.
- Also `"improve the parser"` and `"the improvement ticket is approved, ship it"`.
  `"please improve the retry logic"` is unaffected — `has_word("retry")` hits
  first, so the bug fires precisely when no Coding keyword is present.
- Sharpest because `miser-policy` turns a Reasoning task into an *unbounded*
  `max(tier, Reasoning)`, unlike the Coding false positive which is bounded by a
  guard at `lib.rs:479`.
- Fixed by using `has_word` for the Reasoning list too, with a test that also
  proves genuine reasoning prompts are still detected — the fix must not have
  traded an over-route for an under-route.
- Tests: `prose_is_not_typed_as_a_reasoning_task`,
  `real_reasoning_prompts_are_still_typed_as_reasoning`.

### 12. Any yes/no question about real work was forced to Trivial at 0.95 confidence

Two independent causes, which is why fixing one was not enough.

The short-definitional override's binary row accepted *any* yes/no question up to
60 characters, satisfying none of the three conditions that function documents
("a definitional opener, a brevity constraint, a short request in total"). It set
`scores[0] = 100` and zeroed everything else, so the tier was Trivial at the
0.95 confidence cap — above both the 0.65 tier-floor threshold and the 0.70
verification threshold, so neither the policy floor nor the cascade could
recover it.

- Input: `"true or false: delete every row in prod and rebuild the index?"`.
- Actual: `Trivial`, confidence 0.95, `effective_tier=Trivial` →
  `mistralai/mistral-nemo` at `max_tokens=256`.

With that fixed, the `trivial` tier's *own* binary pattern (worth 5) still
decided it alone: when it is the only match it wins 5-to-0 against a field of
zeros and picks the tier by itself.

- The discriminator is not the binary framing but whether the question asks for a
  *fact* or requests *work*: a leading copula or quantifier
  (`is|are|was|were|does|did|has|have|any|all|every`) keeps it Trivial, an
  imperative does not. `can/could/will/would` are admitted only in the third
  person, because that is what separates "can a primary key be null?" from
  "can you add retries?"; `should` is excluded outright. That last distinction
  needs a negative lookahead and the `regex` crate has none, so it is a small
  function (`is_third_person_modal_question`) rather than a pattern.
- Both patterns are now built from one shared `FACTUAL_OPENER`, because a
  drifting copy of a weaker pattern that decides the tier alone is exactly how
  this under-route survived a fix to one of them.
- Corpus note: the first attempt required a brevity marker instead and cost
  0.5 points on `classifier_cases.jsonl` — the corpus pins
  `"just answer yes or no: is Python interpreted?"` at Trivial, and the corpus was
  right to.
- Tests: `a_binary_question_about_real_work_is_not_capped_as_trivial`,
  `a_binary_question_about_a_fact_is_still_trivial`.

---

## Routing and failover

### 13. Hysteresis could not retire a Reasoning pin that could not reason — `gateway/src/catalog.rs`

`prefer_reasoning_pin` was enforced in `pin_choice` and in the candidate loop, but
not in the "keep the incumbent" arm, which compared price only.

- Defaults (`switch_saving_ratio = 0.25`, `prefer_reasoning_pin = true`);
  incumbent `seeded/non-reasoning` @ $1.00/M; `pool/thinker-a` @ $1.30 (reasoning),
  `pool/thinker-b` @ $1.35. `pin_choice` → `pool/thinker-a`; `1.30 <= 0.75` is
  false, so hysteresis kept the incapable incumbent.
- **Unrecoverable by refresh**: hysteresis only ever migrates to a *cheaper*
  model, and a capable one costs more, so the pin could never move up. The only
  escape was a three-failure failover streak.
- Fixed via a shared `keeps_reasoning_pin` predicate used by all three sites.
- Tests: `hysteresis_does_not_keep_a_non_reasoning_pin_for_the_reasoning_tier`,
  and `hysteresis_still_keeps_the_incumbent_when_it_is_qualified` so the fix is
  shown not to have disabled pin stability on tiers with no capability
  requirement.

### 14. A refresh kept a stale pin's stale candidate list — `gateway/src/catalog.rs`

A tier whose filtered pool is empty keeps its previous pin (documented, sensible).
But the preserved `TierPin` carried a `candidates` list validated against the
catalog the refresh had just superseded, and `report_failure` walks
`candidates[position+1]` unconditionally. So a 5xx streak promoted a successor
the refresh had proven is not a viable candidate. And because a 404 from a
delisted model is classed as a *client* error, `report_failure` was never called
again, so the tier stayed wedged.

- Fixed by retaining only candidates still present in the new catalog. Keeping
  the pin stays deliberate; the stale candidates do not.

### 15. A failed escalation that never completed was invisible to failover — `gateway/src/main.rs`

`if let Ok(escalated_upstream) = state.provider.forward(..)` had no `else`. The
first call's transport error was reported and an escalation returning 5xx was
reported, but a connection that never completed was dropped entirely.

- Mock: call 0 answers normally, call 1 hangs up. Result was 2 upstream calls,
  `200` to the client, and `upstream_errors_total` stuck at 0 — so a model that
  reliably dies on escalated requests could never accumulate the
  `failover_threshold` failures needed to be retired.
- Fixed with the missing `else`. The test uses a raw `TcpListener`, because the
  failure cannot be produced through an HTTP handler.
- Test: `an_escalation_that_never_completes_is_reported_to_the_failover_counter`.

---

## Smaller defects

| # | Defect | Fix |
|---|---|---|
| 16 | An unreadable price became $0.00/M. `unpriceable` only caught a *string* parsing to a non-finite number, so a JSON `null`, an object, a bool, an array, or `"see pricing page"` all fell through to 0.0 — and with `allow_free = true` that made a paid model the *cheapest* candidate in its band. | One `price_per_token` extractor shared by both questions, so "present but unreadable" means the same thing whatever JSON type carried it. A JSON *number* is now honoured rather than dropped. Tests: `an_unreadable_price_is_dropped_whatever_json_type_carried_it`, `a_numeric_json_price_is_honoured`. |
| 17 | `QualityConfig::escalate_on_failure` had two defaults — serde `false`, `impl Default` `true`. Unobservable while `enabled` is false, so the only place it showed was a config naming `[quality]` with `enabled = true` and omitting the key: the documented escalation silently never ran, while dropping the whole table gave `true`. *Naming a table changed the meaning of a key inside it.* | Aligned on `false` — a second, separately billed call should be asked for, not appear because a heading was typed. The test previously recorded the divergence in a comment and asserted the serde value anyway. |
| 18 | `validate_config` could not reject NaN bands. Every `<=`/`>=` is false against NaN, and TOML has a `nan` literal that `toml` 0.9 accepts, so `standard_max = nan` passed all four order checks and the gateway started normally — then `band_tier`'s `<= NaN` tests all failed and every model in the affected price slice fell through to the strongest tier. | Finiteness checked alongside the ordering, matching the message the function already emits. Tests assert both the rejection and the collapse it prevents. |
| 19 | `MISER_CATALOG_FILE` was a fallback, not the override `miser-types:595` documents. `cache_path` was checked first and the shipped `config/miser.toml` sets it, so the documented override was inert in the shipped deployment. | Env var consulted first; an empty value falls through. Test: `the_catalog_file_env_var_overrides_the_configured_cache_path`. |

---

## Recorded, not fixed

These are real, and none is a small fix. Flagged rather than silently patched.

### Quota state is in-memory only — every restart resets every cap

`QuotaEnforcer::new` starts two empty maps; nothing is loaded from disk at boot,
unlike the key store (persisted atomically) and the usage ledger (JSONL). So every
deploy, crash or `systemctl restart` hands every key its entire remaining monthly
budget and a free `rpm` burst — the effective ceiling is `cap × restarts`. The
per-key data to reconstruct it is already written to the ledger on every request,
but note the ledger's `cost_usd` is the real-usage figure while the budget charges
the `max_tokens` estimate, so a reconstruction must use the same formula, not the
recorded cost. This is a deployment/persistence design change, not a patch.

### The verification cascade has no live validation of its question

`answers.tier_is_right.noul` is the only piece of model-authored text in the
codebase with zero live validation: `tests/cascade.rs` feeds a mock the number,
and `live_jev_contract` (the one `#[ignore]`d test that would exercise the real
service) does not cover the cascade's question at all. The `probabilities`
fallback is likewise validated only against hand-written mocks. This is the least
verified and most consequential stage in the classifier — it can escalate traffic
— and it is the stage two of the defects above (13, and the Hybrid bypass) sat
behind.

### Semantic cache similarity is a recall mechanism being used as a safety mechanism

Even partitioned by tenant and parameterised, the cache decides a hit from a
bag-of-words cosine whose threshold is a tuning choice, not a guarantee. LLD §6
records the original author's conclusion — "coding prompts share vocabulary
causing false-positive cache hits at any threshold below 1.0" — and recommends
proper sentence embeddings or exact-match only. The `semantic_enabled` flag is
the real control here; defects 4, 7 and 12 narrowed the blast radius but did not
change that the score is not a correctness argument.
