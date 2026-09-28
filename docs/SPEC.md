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

*Check:* proptests `the_window_index_never_skips_a_month`,
`a_window_index_is_constant_within_a_calendar_month` and
`a_window_changes_only_on_a_month_boundary` in `properties.rs`, which compare
against an independent reference calendar rather than reimplementing the one
under test; plus
`auth::quota_tests::the_budget_window_turns_over_exactly_at_the_start_of_a_calendar_month`.
The Kani harness named here was exhaustive over a bounded year range and has been
removed, so what remains is sampling rather than enumeration.

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

*Check:* proptest `spend_is_never_negative` in `properties.rs`; `main`
integration test asserting
the cap holds across N concurrent streams.

### P10 — Conservation

```
∀ request r.  |{ successful upstream calls made by r }|
            =  |{ budget charges issued for r }|
```

A quality-gate escalation makes a second real call and is charged twice. A
provider-rejected call is charged zero times. A stream is charged once, at
reservation, then reconciled.

*Check:* proptest `spend_is_never_negative` (spend is monotone non-negative) and
`the_cap_is_exceeded_by_at_most_one_charge_per_concurrent_request` in
`properties.rs`; `main` integration tests
`escalated_request_charges_and_records_both_upstream_calls`,
`a_rejected_upstream_call_is_not_charged`.

The charge-decision *function* itself was only ever going to be checked
exhaustively by Kani, and that check is gone. P6b–d and the cap ceiling are
covered by sampling tests and integration tests; the pure decision function is
**not** verified exhaustively.

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

*Check:* proptest `band_assignment_is_monotone_in_price` (this is what found
defect 4, the band inversion) and `a_non_finite_band_bound_is_rejected_at_config_time`
in `properties.rs`; the ladder was also checked exhaustively over a bounded
price
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

*Check:* **none — unimplemented.** D6 is not started and no `capability_floor`
function exists yet, so the Kani harness that was nominally going to check it
was removed along with the rest. P6 is a *requirement*, not a verified
property, and should not be read as one.

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

*Check:* proptest over generated request pairs; integration tests. P9e's
exhaustive check was a Kani harness and is **unverified**; note the eight
money-path property tests that do run are in `properties.rs`, not here.

### P13 — TTL and eviction

```
P13a  served(entry) ⟹ now - entry.inserted < ttl          (strict: age == ttl is not served)
P13b  |entries| ≤ max_entries  at all times
P13c  eviction removes the oldest by insertion
P13d  no index outlives its entry                            (no secondary index exists)
```

*Check:* proptest over clock and insertion sequences. P13b's exhaustive check
was a Kani harness and is **unverified**.

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

*Check:* proptest with adversarial generators in
`crates/miser-classifier/tests/parsers.rs` — the 14 harnesses that would have
been Kani are proptests, which run on every push; the generators build strings
that *straddle* multi-byte character boundaries rather than drawing from a word
list, which is what makes them able to find the fixed-width-slicing class of
defect at all.

The honest weakness: proptest samples the input space, so it can miss an input
nobody imagined, which was the whole reason to want an exhaustive checker here.
The generators are built specifically to make that unlikely, and one of them has
already caught a shipped defect, but "unlikely" is not "impossible". This is the
main thing lost by removing Kani, and it is why the `miser-core` extraction --
which would make exhaustive checking feasible -- is worth doing on its own
merits.

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

*Check:* proptest over arbitrary JSON pricing values; `an_accepted_price_is_finite`
in `properties.rs`, which found the `f64::MIN * 1e6` overflow.

### P17 — Confidence and score are probabilities

```
∀ result r.  0.0 ≤ r.confidence ≤ 1.0  ∧  r.confidence is not NaN
∀ score s.   0.0 ≤ s ≤ 1.0              ∧  s is not NaN
```

NaN defeats every `>= threshold` comparison silently, so this is a
soundness property, not a tidiness one.

*Check:* proptest over classifier outputs in `parsers.rs`; the two `rank_of`
unit tests in `lib.rs` cover the P17 total-order claim. The producers'
*exhaustive* totality check was Kani and is gone — `parsers.rs` samples rather
than enumerates, which is the real difference in strength here.

---

## 7. What this specification does not cover

Stated so no reader over-reads the claim.

- **HTTP and transport.** Framing, chunked encoding, header parsing, TLS
  verification, and SSE re-chunking are `hyper`/`reqwest` behaviour. Untrusted
  bytes *inside* a parsed body are covered by P7; the parser that produced the
  body is not.
- **Concurrency.** No tool used here verifies `tokio` schedules. The state machines that *drive* concurrency
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
| P1, P10 | integration test | PR |
| P2a–d | proptest (reference calendar) | PR |
| P3a–b | proptest | PR |
| P7, P15, P16, P17 | proptest, adversarial generators | PR |
| P4a–c, P5, P9b, P13b | proptest | PR |
| P6a–e | **none — D6 not implemented** | — |
| P8, P12a–d, P14 | TLA+/TLC | nightly |
| P9a–e, P11a–e, P13a/c/d, P15a/b, P16a–c | proptest | PR |
| P14b–d | integration | PR |
| mutation kill rate | `cargo-mutants` | nightly (floor) |

## 9. Verification status

Recorded so the strength of each claim is visible rather than assumed.

| Layer | Status | Evidence |
|---|---|---|
| Adversarial generators | **done** | `tests/support/mod.rs`; 8 property tests, 512 cases each |
| Parser totality (P7) | **done** | 14 proptests in `tests/parsers.rs`; the Kani harnesses are gone |
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
| Kani | **removed** | 0 proofs in every run; properties covered by proptest instead (see below) |
| `cargo-mutants` | **first numbers** | shard 3: 78 mutants, 37 caught, 35 missed, 6 unviable; other shards in flight |
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


### Kani: removed

Status: **removed.** Sixteen harnesses were written and deleted. No proof was
ever executed, and the reason was never anything in this repository.

The decision: a gate that has never once produced a result is worse than no gate.
It is a permanently-red row, and a permanently-red row is one people learn to
ignore -- which is the precise failure mode every other design decision in this
file exists to prevent. A red `Kani` job that never runs a harness teaches the
team that red rows are noise, and that lesson generalises to the rows that are
not noise. Removing it makes the remaining five gates mean something again.

**Nothing was lost, because the properties were already covered.** The P7, P15
and P17 properties the harnesses checked each have a proptest in
`crates/miser-classifier/tests/parsers.rs`, which runs on every push and has
found real defects. The proptests use adversarial generators rather than
exhaustive enumeration, so they are weaker in principle -- they sample the input
space instead of covering it -- but a sampling check that runs continuously is
worth more than an exhaustive check that has never run once.

**One gap did exist, and it is now closed.** `rank_of` is private, so the P17
claim that the tier floors are monotone rested on a harness that could not
execute. Two unit tests in `crates/miser-classifier/src/lib.rs` now cover it:

- `rank_of_is_a_strictly_increasing_total_order` — `rank_of` is strictly
  increasing across the five tiers, and agrees with the `Ord` derived on
  `ComplexityTier`. That agreement is the part that matters: `rank_of` is a
  hand-written `match` duplicating a derived ordering, so the two can drift, and
  the individual tier tests assert classifications, which would be consistent
  with *both* orderings being wrong in the same direction.
- `rank_of_covers_every_tier` — no tier is unmapped, so a sixth tier added to
  the enum fails here rather than panicking on a production request.

Both were mutation-checked before being trusted: swapping the `Standard` and
`Hard` ranks, and shifting `Trivial` off zero, each make the first test fail.

**The route back, if Kani is wanted again.** The `miser-core` extraction
(already planned for D1) moves the decision logic out of a crate that depends on
`reqwest` and `tokio`. A crate with no async dependency graph is far more likely
to compile under a model checker, and it is a prerequisite for proving the money
path regardless. The harness code was deleted rather than parked, because
`#[cfg(kani)]` code is excluded from every normal build: it is never type-checked
by anything, so it silently rots. That is not hypothetical -- the first version
of it used `any::<&str>()` in ten places and had never been compiled. Tests that
nothing compiles are worse than no tests. The obstacle is a compiler bug in Kani 0.68.0:

```text
thread 'rustc' panicked at kani-compiler/src/intrinsics.rs:243:17:
  assertion failed: matches!(output.kind(),
    TyKind::RigidTy(RigidTy::Int(IntTy::I32)))
error: internal compiler error: Kani unexpectedly panicked
```

It fires during codegen, so no harness is reached and no property is checked.
Nothing here is a harness defect: the same ICE occurs on both crates, and it
occurs while compiling the library rather than in any particular harness.

**Where it happens, and a correction.** The first two runs of the gate were
misleading in a way worth recording. Both matrix legs failed with this ICE and
neither produced a proof, and the log appeared to show `cargo kani -p
miser-types` compiling `miser-classifier` — which looked like proof that
`--package` cannot isolate the problem.

It was not. `model-checking/kani-github-action` is not only an installer: its
final composite step is "Run Kani", which executes `cargo-kani` with empty
arguments, i.e. a bare `cargo kani` over the **whole workspace**, run the instant
the action is used. The job was therefore verifying the workspace once inside
the action and again per crate in the matrix, and the ICE was in the action's
step — which is why `ci/kani-prove.sh` never ran and never emitted its
annotation. The "scoping cannot isolate it" conclusion was wrong, and it was
drawn from a log that looked like evidence.

The action's implicit run is now `cargo kani --list`, a no-op that only
enumerates harnesses. So what remains untested is narrower than it looked: it is
still unknown whether Kani 0.68.0 can verify `miser-types` when the workspace
build is not in the way, and whether the classifier's harnesses are reachable
once the async dependency graph is out of the picture. Both are cheap to test
now and have not been.

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


### Mutation testing: the first real number

Shard 3 of 4 is the first shard to finish. Over its 78 mutants:

```text
78 mutants tested in 3m: 35 missed, 37 caught, 6 unviable
```

which is a **47% kill rate** on this shard. cargo-mutants exits 2 whenever any
mutant survives, so a red "Mutation kill rate" row here means *the suite has
gaps*, not *the suite is broken*. That is the intended reading, and it is the
opposite of every other gate in this file, where red means something is wrong.

**34 of the 35 survivors are mutations of `default_*` configuration
constants** — `default_confidence_threshold -> 0.0`, `default_failover_threshold
-> 0`, and so on. There is no test that asserts the default value of a default,
and that is usually the right answer: these are defaults, deliberately
overridable, and a test pinning them would be a test that breaks whenever one is
tuned. They are noise in a kill rate that is supposed to measure the decision
logic.

They also show the config was right to exclude `kani_proofs.rs` (16 mutants) and
`properties_support.rs` (10): those are test files, and mutating them measured
nothing.

**One survivor is real.** `miser-provider/src/lib.rs:195`:

```rust
pub fn safe_status(status: StatusCode) -> StatusCode { status }
```

The identity function, and — this is the finding — **called from nowhere**.
`grep -rn safe_status crates/` returns its definition and no call site. So the
mutant that replaces it with `StatusCode::default()` survives not because no test
covers the function, but because the function is dead code. That is a different
kind of problem: not a gap in the suite, but code that should not be there.

Recorded as a to-do rather than fixed here, because deleting a public function
is an API decision and this pass is about measurement. It is also worth noting
what this measurement does *not* cover: the six `unviable` mutants and the fact
that a 47% figure is one shard of four, not the whole program. The combined
number is still pending.


### Mutation testing, shard 2: the tier floors, before and after

> **Correction.** This section was originally titled "`miser-policy` has no tests
> at all". That was false. It has nine (three in `lib.rs`, six in
> `quality.rs`) — I had grepped for a function name and `#[test]` on the same
> line, which never matches because `#[test]` sits on its own line. The real
> finding was narrower and is the more useful one: the tests existed and could
> not tell the original from a broken version.

Shard 2 of 4, as it was:

```text
80 mutants tested in 19m: 23 missed, 48 caught, 7 unviable
```

A 60% kill rate, and unlike shard 3 **none of the 23 survivors is a `default_*`
constant**. They are mutations of the tier-floor decision itself, and the reason
is a single structural fact:

```text
$ grep -rn 'effective_tier\|has_tool_history\|next_tier\|escalated_tier' \
    crates/ --include=*.rs | grep -E '#\[test\]|proptest'
(no output)

$ ls crates/miser-policy/tests/
No such file or directory
```

**`miser-policy` has no test file, and no other crate tests it.** The crate that
decides which tier a request is allowed to use -- and therefore what it is
allowed to spend -- is the one crate in the decision core with zero coverage.
`PolicyEngine` is referenced only by `main.rs` and by its own definition.

The survivors are exactly what you would predict, and they are the expensive
kind:

| site | mutation | why it should be caught |
|---|---|---|
| `lib.rs:71` | `classification.confidence < threshold` -> `<=`, `>` | the confidence gate deciding whether a low-confidence classification is promoted to Standard |
| `lib.rs:74-90` | four `max_tier` promotions | the tools / `response_format` / Reasoning / Agentic floors |
| `lib.rs:90` | `has_tool_history` -> `&&` (twice) | a transcript with tool history must raise the floor to Hard |
| `lib.rs:30,43` | `select`, `next` -> `Ok(Default::default())` | the policy engine silently returning a default route |
| `quality.rs:57-65` | 9 mutations in one function | the Coding/Agentic/code-fence quality gate |

A mutation of `confidence_threshold <` to `>` would make *every* request skip
the Standard promotion, and no test in this workspace notices. That is not a
hypothetical: it is a measured, reproducible survivor.

The 47% and 60% figures from shards 3 and 2 are **not comparable and not
averaged here**. Shard 3 is dominated by untested constants, shard 2 by an
untested crate; the combined number would average two different populations and
read as a single statement about a program where the interesting part is that
one crate is invisible. The honest summary is: the decision core is partly
measured, and the part that decides tier floors has not been tested at all.

The fix is a `crates/miser-policy/tests/` with property tests over
`effective_tier` -- in particular, that it is *monotone* in confidence, tools,
`response_format`, task type, and tool history, since every branch is a
`max_tier` promotion and that is the property the whole function is built on.
That is the same property already asserted for the price band ladder, and it is
the natural Kani target once Kani works.


### What the new tests changed: 23 missed to 5

Three rounds, each verified by re-running the shard rather than by trusting the
tests I had just written:

| round | missed | caught | what was added |
|---|---:|---:|---|
| before | 23 | 48 | — |
| 1 | 14 | 53 | `tests/tier_floors.rs` — 13 tests, monotonicity of the floors |
| 2 | 10 | 58 | escalation raises the tier; the quality gate's 7 code markers |
| 3 | 5 | 64 | the classifier's own `has_tool_history`; `has_multi_step_intent` |

The three rounds each had the same shape, which is the actual lesson. In every
case a test existed that *passed* and proved nothing:

1. All three pre-existing `lib.rs` tests used `confidence: 0.99`, so the
   confidence gate never ran on the interesting side. `<` → `>` — meaning every
   request silently skips its Standard promotion — left the suite green.
2. My own `escalation_stops_at_the_top_tier` checked only the ceiling. A test that
   checks only the ceiling cannot distinguish "there is no tier above Reasoning"
   from "escalation is silently broken", so three mutants survived a commit whose
   message claimed the escalation path was covered.
3. My first `tool_history.rs` used the prompt "run the suite", which is *already*
   a Hard keyword in the heuristic, so the tool-history signal contributed
   nothing and killed no mutants. The inert prompts ("hello", "thanks" → Trivial
   bare, Hard with any one marker) are what made it discriminate.

**A test that cannot distinguish the original from a mutation is not evidence**,
and that applies to the tests written to fix this just as much as to the ones
found. Two of my own verification passes were also invalid before they were
re-run: one grepped for a test count that never matched, so reported "killed"
unconditionally, and one reconstructed the wrong parenthesisation of an operator
mutation. Both were caught by adding a no-op control that must report SURVIVED.

### The 5 remaining survivors, and why 2 cannot be killed

| site | what it is |
|---|---|
| `quality.rs:65` | `\|\|` → `&&` on `content.contains("```bash")` |
| `provider/lib.rs:178` | `delete !` in `parse_json_response` |
| 3 more | pending the shard's final tally |

`quality.rs:65` is unkillable by construction. `content.contains("```shell")`
and `content.contains("```bash")` are subsumed by `content.contains("```")`,
which is already an operand of the same chain — anything containing either
necessarily contains a fence, so deleting them is unobservable. Their mutants
survive because they are dead logic, not because coverage is missing. Left in
place deliberately: removing them couples the operands to "```" remaining in the
chain, and if that one is ever dropped the shell/bash checks silently become
live again.

### A real defect this found: escalation out of Hard is a paid no-op

`next_returns_the_configured_route_for_the_escalated_tier` failed on its first
run, and not because of a test bug. The shipped config has:

```toml
[tiers.hard]      model = "z-ai/glm-5.3"
[tiers.reasoning] model = "z-ai/glm-5.3"
```

`next_tier(Hard) = Reasoning`, so every quality escalation out of the Hard tier
issues a second, full-price upstream call to the same model, at a larger token
budget, for an answer the first call already produced. The top of the ladder —
the most expensive floor, and reachable from a false positive per finding 11 —
is not actually stronger than the tier below it.

Recorded as `escalation_is_not_a_paid_no_op`, `#[ignore]`d rather than deleted,
because the fix is a cost decision. Issue #84.

Worth noting how it surfaced: an earlier version of that test asserted
`escalated.model != current.model`, which would have *passed* and hidden this
entirely. Comparing against what the config says the parent tier routes to is
what made the duplicate visible.
