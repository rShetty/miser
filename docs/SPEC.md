# Miser — formal specification

**This document is the oracle.** Every property below is mechanically checked by a
named tool, and every check cites its property ID. A property with no
corresponding check is a wish, not a specification, and is marked as such.

Scope of the claim: properties here cover the **decision core** — tier
selection, money, quotas, authorization ordering, cache identity, and the pure
parsers. The I/O surface (HTTP framing, TLS, SSE transport, filesystem
persistence, `tokio` scheduling) is **not** covered and cannot be with current
tools. That boundary is stated rather than implied; see §7.

Notation: `rank(Trivial)=0 … rank(Reasoning)=4`. `∀` over the domain stated.

---

## 1. Resolved decisions

These seven were undecided — code and documentation disagreed, and three carried
money consequences. Each is now fixed, and the property that pins it is listed.

| # | Decision | Pinned by |
|---|---|---|
| D1 | **Quota state is reconstructed from the usage ledger at boot.** The ledger records `budget_charged_usd` per call — the amount actually charged to the budget — so reconstruction is exact and auditable. A restart must not restore a cap. | P1, P10 |
| D2 | **The budget window is the UTC calendar month.** Field name, the `402 monthly budget exhausted` response, and the docs all say calendar month. | P2 |
| D3 | **The semantic cache defaults to off.** LLD §6 recorded it as disabled for false-positive hits; `config/miser.toml` contradicted that with `semantic_enabled = true`. When enabled it is partitioned per tenant and gated on the tier allowlist. | P9, P12 |
| D4 | **Price bands are ordered by tier strength.** A `hard_max` band is added so the ladder is trivial ≤ simple ≤ standard ≤ hard ≤ reasoning, replacing the arrangement where the Reasoning band sat *below* Hard. | P4 |
| D5 | **`max_cost_per_1m` is deleted.** It was parsed and read by nothing. Unimplemented configuration is a trap: an operator sets it and believes it is enforced. | P5 |
| D6 | **Capability floors are enforced per request.** `vision` and `json_mode` were declared and never checked, and `require_tools` was a config-time all-tiers filter rather than a per-request one. LLD §5 already promised this. Fail-closed: reject rather than serve an incapable model. | P6 |
| D7 | **Streaming may exceed the cap by the final in-flight request.** A reservation is made before the answer exists; the real `usage` arrives afterwards and is reconciled. Bytes already sent cannot be unsent, so the cap is soft by exactly one request's overrun. Documented, bounded, and asserted. | P8 |

---

## 2. Quotas and money

### P1 — A restart does not restore a cap

```
∀ key, t_boot.  spend_after_boot(key)
              = Σ  r ∈ ledger(key) ∧ window(r.ts) = window(t_boot) :  r.budget_charged_usd
```

Reconstruction uses `budget_charged_usd`, **not** the ledger's `cost_usd` field.
The two differ by design: `cost_usd` is the provider's real usage for reporting,
while the budget is charged an estimate and then reconciled. Reconstructing from
`cost_usd` would silently under- or over-count the cap.

*Check:* `auth::quota_tests::boot_reconstruction_reproduces_live_spend` (integration,
real restart of the enforcer against a real ledger), plus P10 at runtime.

### P2 — The window is the UTC calendar month

```
∀ t.  window(t) = year_utc(t) * 12 + month_utc(t)          (month0 ∈ [0,11])

P2a  ∀ t, t'.  month_utc(t) = month_utc(t') ⟹ window(t) = window(t')
P2b  ∀ t.  window(t + 1s) ≥ window(t)                     (monotone)
P2b' ∀ t.  window(t + 1s) > window(t)  ⟹  t is the last second of a month
P2c  ∀ t.  window(t) ≤ window(t + 31 days)                (cannot skip a month)
P2d  window(t) changes  ⟺  t is the final second of a UTC calendar month
```

P2c is the one that would have caught the original `secs / 2_629_800`: a 30.44-day
window skips February in non-leap years, so `window(t) ≤ window(t+31d)` fails for
`t` in early March of such a year.

*Check:* Kani harness `calendar_month_index_properties` (exhaustive over a bounded
year range) + `auth::quota_tests::the_budget_window_turns_over_exactly_at_the_start_of_a_calendar_month`.

### P3 — Spend is never negative, and a cap is never exceeded by more than one call

```
P3a  ∀ key.  spend(key) ≥ 0
P3b  ∀ key, n concurrent.  spend(key) ≤ cap(key) + n · max_single_call_charge
```

P3a matters because reconciliation is a *negative* adjustment: a reservation
followed by a real-usage correction. Without a floor, a key could be handed
negative credit, which `check_budget` would read as headroom.

P3b is a check-then-act bound, not a bug: the check and the charge are separate
lock acquisitions. The bound is `concurrency_limit ×` the largest single charge.

*Check:* Kani `quota_adjust_spend_never_negative`; `main` integration test asserting
the cap holds across N concurrent streams.

### P10 — Conservation

```
∀ request r.  |{ successful upstream calls made by r }|
            =  |{ budget charges issued for r }|
```

A quality-gate escalation makes a second real call and is charged twice. A
provider-rejected call is charged zero times. A stream is charged once, at
reservation, then reconciled.

*Check:* Kani on the pure charge-decision function; `main` integration tests
`escalated_request_charges_and_records_both_upstream_calls`,
`a_rejected_upstream_call_is_not_charged`.

---

## 3. Authorization

### P8 — Cached and fresh responses are controlled identically

This is the general statement of the defect class behind finding 3, where the
rate-limit and budget checks had been hoisted above the cache and the tier
allowlist had not.

```
∀ request q.  controls(q) = controls(q')   whenever response(q) = cached(q')
```

where `controls` is the set of authorization and quota decisions applied. A cache
hit must not be a path around a control that a miss would have applied.

*Check:* TLA+ spec `AuthorizationOrdering.tla` — exhaustively explores the
handler's control order and asserts no control is reachable only on the miss
path. This is a *state-space* property, so it covers orderings not enumerated by
hand.

### P12 — A cache hit is authorized

```
P12a  ∀ hit served to key K.  entry.tenant = K.id
P12b  ∀ hit served to key K.  K.allowed_tiers = ∅ ∨ entry.tier ∈ K.allowed_tiers
P12c  ∀ hit served to key K.  check_rate_limit(K) was evaluated for this request
P12d  ∀ hit served to key K.  check_budget(K) was evaluated for this request
```

P12a is tenant isolation. P12b is the tier gate. P12c/d are already satisfied by
the current ordering and are asserted so a future reordering cannot lose them.

*Check:* integration tests `a_semantic_hit_never_crosses_api_keys`,
`a_cached_entry_is_not_served_to_a_key_barred_from_its_tier`, plus the TLA+ spec.

### P14 — A revoked or expired key is never served

```
P14a  ∀ request q with K.active = false.  ¬served(q)
P14b  ∀ request q with K.expires_at < now.  ¬served(q)          → 403
P14c  rotate(K) does not change K.active                       (no resurrection)
P14d  revoke(K) → ∀ future q.  ¬served(q)
```

*Check:* existing `auth::tests` for P14c/d; integration test for P14b's status code.

---

## 4. Routing

### P4 — The tier ladder is monotone in price

```
∀ p.  band(p) = the greatest tier t such that price ≤ bound(t)
P4a  0 < trivial_max < simple_max < standard_max < hard_max
P4b  ∀ p1 ≤ p2.  rank(band(p1)) ≤ rank(band(p2))
P4c  ∀ bound.  bound is finite
```

P4b is what the original arrangement violated: `reasoning_max` was tested before
the `Hard` fallthrough, so the Reasoning tier was fed models *cheaper* than Hard's.

P4c: every `<=` is false against NaN, and TOML accepts a `nan` literal. Without
the finiteness clause, `standard_max = nan` passed all four order checks and then
failed every band comparison open to the strongest tier.

*Check:* Kani `band_tier_is_monotone_and_total` (exhaustive over a bounded price
range × bounded bounds) + `validate::tests::rejects_non_finite_band_bounds`.

### P6 — Capability floors

```
P6a  ∀ request q.  ∀ c ∈ required_capabilities(q).  selected(q) supports c
P6b  q has tools declared, or tool history ⟹ selected(q).tools
P6c  q has response_format ⟹ selected(q).json_mode
P6d  q has an image content part ⟹ selected(q).vision
P6e  no model in the catalog satisfies required_capabilities(q) ⟹ 422, never a weaker model
```

P6e is the fail-closed clause: a 422 is a correct outcome, silently downgrading
the request is not.

*Check:* Kani on the pure capability-floor function; integration tests for P6b–d
in both config and catalog mode.

### P5 — No dead configuration

```
∀ field f in RoutingConfig ∪ TierModelRouteConfig.  f is read by the gateway
```

*Check:* `config::tests::every_routing_config_field_is_read` — a checked-in list
of field names asserted against the fields the gateway actually consults, so
re-adding an unread field fails the build.

### P11 — No silent under-route

```
P11a  ∀ q.  effective_tier(q) ≥ classified_tier(q)
P11b  ∀ q.  confidence(q) < threshold ⟹ effective_tier(q) ≥ Standard
P11c  ∀ q.  task(q) = Reasoning ⟹ effective_tier(q) = Reasoning
P11d  ∀ q.  override present ⟹ effective_tier(q) = override tier, unmodified
P11e  ∀ q.  effective_tier(q) ≥ cascade_floor(q)
```

P11a/P11b are the floor property: verification and context signals may raise a
tier, never lower one. P11c is the unbounded `Reasoning` floor that a substring
keyword collision could trigger.

*Check:* proptest over generated requests asserting P11a–e; corpus gates for
labelled accuracy.

---

## 5. Caches

### P9 — A cache key is a fingerprint of the request

```
P9a  hit(entry, q) ⟹ embedding_text(entry) = embedding_text(q)
P9b  ∀ p ∈ {max_tokens, max_completion_tokens, temperature, top_p, stop}.  p present in q
                    ⟹ p ∈ embedding_text(q)
P9c  hit(entry, q) ⟹ tenant(entry) = tenant(q)
P9d  lookup never returns an entry whose tenant differs
P9e  ¬hit(entry, q) when max_entries = 0
```

P9b is finding 7: message text alone made generation parameters invisible, so an
8-token request was served a 4096-token answer. P9e is finding 2: capacity zero
is the documented "off" switch and used to panic.

*Check:* proptest over generated request pairs; Kani for P9e; integration tests.

### P13 — TTL and eviction

```
P13a  served(entry) ⟹ now - entry.inserted < ttl          (strict: age == ttl is not served)
P13b  |entries| ≤ max_entries  at all times
P13c  eviction removes the oldest by insertion
P13d  no index outlives its entry                            (no secondary index exists)
```

*Check:* proptest over clock and insertion sequences; Kani for P13b.

---

## 6. Parsers

### P7 — No panic on untrusted input

```
∀ input x.  parse(x) does not panic
```

Checked for: `@route:` prefix probe, `has_word`, `is_short_definitional`,
`request_text_for_embedding`, `usage_from_sse`, `strip_code_fence`, the catalog
price extractor, `content` deserialization, `window_since`.

This is the class behind findings 1 and 2 — fixed-width byte slicing and
`Vec::remove(0)` on a possibly-empty vector. Both are *impossible to find by
reading* and trivial to find symbolically.

*Check:* Kani harnesses per parser, each taking `kani::any::<&str>()` or
`kani::any::<&[u8]>()` and asserting termination. This is the single
highest-value use of Kani here, because it is the only method that covers inputs
nobody imagined.

### P15 — An unrecognised field survives a round trip

```
P15a  ∀ part ∉ {text, image_url, input_audio, refusal}.  serialize(parse(part)) = part
P15b  ∀ known part.  the `type` tag survives serialization
```

P15a is finding 5, where an internally tagged *unit* variant discarded both the
tag and the payload and re-emitted `{"type":"Other"}`.

*Check:* proptest generating arbitrary JSON parts; a round-trip test per known shape.

### P16 — Prices are never invented

```
P16a  price key present and unreadable  ⟹  the model is dropped
P16b  price key absent                  ⟹  0.0, gated by allow_free
P16c  ∀ accepted model.  input_price_per_m is finite
```

P16a is finding 16: a JSON `null`, object, bool, or non-numeric string became
$0.00/M, which with `allow_free = true` made a paid model the *cheapest*
candidate in its band.

*Check:* proptest over arbitrary JSON pricing values; Kani on the extractor.

### P17 — Confidence and score are probabilities

```
∀ result r.  0.0 ≤ r.confidence ≤ 1.0  ∧  r.confidence is not NaN
∀ score s.   0.0 ≤ s ≤ 1.0              ∧  s is not NaN
```

NaN defeats every `>= threshold` comparison silently, so this is a
soundness property, not a tidiness one.

*Check:* proptest over classifier outputs; Kani on the producers.

---

## 7. What this specification does not cover

Stated so no reader over-reads the claim.

- **HTTP and transport.** Framing, chunked encoding, header parsing, TLS
  verification, and SSE re-chunking are `hyper`/`reqwest` behaviour. Untrusted
  bytes *inside* a parsed body are covered by P7; the parser that produced the
  body is not.
- **Concurrency.** Kani does not support Rust concurrency and no other tool
  verifies `tokio` schedules. The state machines that *drive* concurrency
  (failover counters, quota maps, cache eviction) are modelled in TLA+ as
  interleavings, which is an abstraction of the real schedule, not the schedule.
- **Filesystem persistence.** Atomic rename, torn writes, and crash recovery are
  modelled by construction (write-temp-then-rename) and by the corruption tests
  (`corrupt_key_store_is_rejected_rather_than_silently_emptied`), not proven.
- **The provider and the classifier model.** OpenRouter's and TypeSafe's actual
  behaviour. `tests/jev_contract.rs::live_jev_contract` is `#[ignore]`d and needs
  `JEV_API_KEY`. The cascade's question — the only model-authored prompt in the
  codebase, and the one that escalates traffic — has no live validation at all.
- **Cost figures themselves.** `price_per_1k_usd` is operator-supplied. The
  arithmetic is verified; the number is not.

## 8. Check-to-property map

| Property | Tool | Blocking |
|---|---|---|
| P1, P10 | integration test + Kani | PR |
| P2a–d | proptest (reference calendar) + Kani | PR |
| P3a–b | proptest | PR |
| P7, P15, P16, P17 | Kani + proptest | PR |
| P4a–c, P5, P6a–e, P9b, P13b | Kani + proptest | PR |
| P8, P12a–d, P14 | TLA+/TLC | nightly |
| P9a–e, P11a–e, P13a/c/d, P15a/b, P16a–c | proptest | PR |
| P14b–d | integration | PR |
| mutation kill rate | `cargo-mutants` | nightly (floor) |

## 9. Verification status

Recorded so the strength of each claim is visible rather than assumed.

| Layer | Status | Evidence |
|---|---|---|
| Adversarial generators | **done** | `tests/support/mod.rs`; 8 property tests, 512 cases each |
| Parser totality (P7) | **done** | 9 tests in `tests/parsers.rs`, plus 12 Kani harnesses |
| Money path (P2, P3, P16) | **done** | 8 property tests in `src/properties.rs` |
| Quota parsing (D1 surface) | **done** | generated test over `patch_field` |
| P4 band ladder | **done** | `band_assignment_is_monotone_in_price` — found D4 |
| P8 authorization ordering | **done, machine-checked** | `spec/AuthorizationOrdering.tla`, TLC 25 states |
| P16 price finiteness | **done** | found a defect introduced by an earlier fix |
| D4 band inversion | **implemented** | `hard_max` added; ladder now monotone |
| P1 quota persistence (D1) | **not started** | needs the `miser-core` extraction |
| D3 cache default off | **not started** | — |
| D5 delete `max_cost_per_1m` | **not started** | — |
| D6 capability floors | **not started** | — |
| D7 streaming overrun | **documented only** | the reconciliation already implements it |
| Kani | **blocked, 0 proofs** | 16 harnesses written; Kani 0.68.0 ICEs compiling `miser-classifier` (see below) |
| `cargo-mutants` | **running** | 318 mutants; a full run is hours |
| `miser-core` extraction | **not started** | prerequisite for proving the money path |

### What the model checker actually established

Not "the model passes" — the specific result:

- `spec/AuthorizationOrdering.cfg` (`CacheConsultsTierGate = TRUE`, the fixed
  handler): **no error**, 25 states explored, no violation of any of the four P8
  invariants.
- `spec/AuthorizationOrderingPrefix.cfg` (`FALSE`, the defect): **violates
  `P8_TierGateBeforeCache`**, with the counterexample

  ```text
  tierAllowed = FALSE          the key is barred from the serving tier
  consulted   = {Auth, RateLimit, Budget}
  quotaOk     = TRUE
  servedFromCache = TRUE
  ```

  which is the reachable state in which a cache hit is served with the tier
  gate never consulted. That is defect 3, as a machine-generated witness.

Both configurations are checked in, and CI runs the second one with its result
**inverted**: a pass there means the model has stopped discriminating, which
would make the first run meaningless.

### The vacuous-pass trap

An earlier version of the model enumerated all 120 orderings of the controls
rather than pinning one, which is strictly more powerful. The set comprehension
needed to filter them evaluated to the **empty set**, and TLC reported

```text
Model checking completed. No error has been found.
0 states generated, 0 distinct states found
```

— a green run that explored nothing. The order is therefore pinned as a
definition in the spec, where changing it is a reviewed source change, and the
weaker-but-honest model is what is checked. Recorded here because a gate that
explores zero states is worse than no gate: it looks green.

### Two defects the property tests found

Neither was visible to review, and the first was introduced by a fix from the
earlier pass:

1. **The price extractor could still make a paid model the cheapest.** The
   finiteness check was on the per-token value, but the ×10⁶ scaling to
   per-million can itself overflow: `f64::MIN * 1e6` is `-inf`, and every band
   comparison is then false while `input_price_per_m <= 0.0` is true — so the
   model read as *free*, banded Trivial, and pinned as the cheapest thing in
   the catalog. Found by `an_accepted_price_is_finite`.
2. **An integer content-part tag was read as a variant index.** With
   `#[serde(untagged)]` over an internally tagged inner enum, serde also accepts
   its *sequence* form, where element 0 is the tag — and there an integer tag
   selects a variant by position. So `content: [[0, ""]]` decoded to
   `Text { text: "" }` and was forwarded as `{"type":"text","text":""}`: a
   malformed part silently replaced by a different, well-formed one. Same class
   as the `{"type":"Other"}` rewrite in finding 5. Fixed by hand-writing
   `Deserialize` so the rule is "an object whose `type` is one of four known
   strings, else verbatim", which makes P15a structural rather than emergent.
   Found by `an_arbitrary_content_part_round_trips_verbatim` on its first run.


### Kani: blocked, and the scope-narrowing plan does not work

Status: **zero proofs have been executed.** Sixteen harnesses are written
(`miser-types` 7, `miser-classifier` 9) and are believed to be correct, but none
has been run. The obstacle is a compiler bug in Kani 0.68.0:

```text
thread 'rustc' panicked at kani-compiler/src/intrinsics.rs:243:17:
  assertion failed: matches!(output.kind(),
    TyKind::RigidTy(RigidTy::Int(IntTy::I32)))
error: internal compiler error: Kani unexpectedly panicked
```

It fires during codegen, so no harness is reached and no property is checked.
Nothing here is a harness defect: the same ICE occurs on both crates, and it
occurs while compiling the library rather than in any particular harness.

**The earlier plan to scope the job to `miser-types` is refuted by
measurement.** The reasoning was that `miser-classifier` is the crate that ICEs,
so proving `miser-types` alone would at least produce a result. It does not:
`cargo kani -p miser-types` still compiles `miser-classifier` — the log shows
`Compiling miser-classifier` inside the `miser-types` job, and the ICE
`could not compile miser-classifier` from that job. Both matrix legs therefore
fail identically and neither yields a proof. Scoping by `-p` cannot isolate the
problem, because the whole workspace is built regardless.

This is the fifth instance of the same failure mode, and the most consequential
one. A Kani job that reports success while proving nothing is worse than no
gate, because the harnesses exist and are believed good; the absence of proofs
is then indistinguishable from their absence as a design decision. So
`ci/kani-prove.sh` requires Kani's `N verification harnesses` summary line and
fails if it is absent, distinguishing the three reasons it can be missing (ICE,
empty harness set, counterexample). A green Kani row now means proofs ran.

**What is left to try**, none of it verified:

1. A different Kani version. Untested, because the version is now pinned and a
   bump is meant to be a deliberate commit carrying the ICE text.
2. Splitting the decision logic into `miser-core` (already planned, §D1). This is
   the principled fix: `miser-classifier` depends on `reqwest` and `tokio`, and
   a large async dependency graph is a plausible trigger for a codegen ICE in an
   intrinsic. A crate with no async dependencies is both provable and easier to
   reason about, which is why the extraction is the prerequisite anyway.
3. Filing the ICE upstream with the exact command line and the `--allow-escape`
   log. The job is pinned at 0.68.0 so an upstream fix shows up as a deliberate
   version bump rather than as a mystery.

Until one of those lands, every property in §"Properties" whose only check is
Kani is **unverified**, and this document should not be read as claiming
otherwise. The properties checked by proptest, integration tests, and TLC are
unaffected and did run.
