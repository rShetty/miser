#!/usr/bin/env bash
# Commit the audit work in reviewable slices, then push.
#
# One commit per coherent change, each referencing the issues it closes, so the
# history reads as the audit trail rather than as one 4000-line blob. `Closes`
# only on issues whose fix is complete and whose regression test is committed.

set -euo pipefail
cd "$(dirname "$0")/.."

DRY_RUN=0
[ "${1:-}" = "--dry-run" ] && DRY_RUN=1

run() {
  if [ "$DRY_RUN" = "1" ]; then printf '\n>>> %s\n' "$*"; else "$@"; fi
}

# Each commit must contain only the files it names. Without this, anything left
# in the index by an earlier `git add` is swept into whichever commit runs
# first -- which is exactly what happened the first time this script ran, and it
# produced one commit titled "fix(types)" containing the entire audit.
run git reset -q

git add crates/miser-types/src/lib.rs crates/miser-types/Cargo.toml crates/miser-types/src/kani_proofs.rs
run git commit -q -m "fix(types): preserve unmodelled content parts byte-for-byte

An internally tagged \`#[serde(other)]\` unit variant has nowhere to keep the
original, so the \`type\` tag and every field on the part were dropped and it was
re-serialised as \`{\"type\":\"Other\"}\`. Any part outside
{text, image_url, input_audio, refusal} -- \`file\`, \`input_file\`, Anthropic
\`thinking\`, Gemini \`inline_data\` -- reached the provider as that, while the
client got a 200.

Deserialize is now hand-written so the rule is structural: a part is Known only
if it is an object whose \`type\` is one of the four known strings, and Other
otherwise, unchanged.

Also aligns QualityConfig::escalate_on_failure, whose serde default (false) and
impl Default (true) disagreed -- naming the [quality] table changed the meaning
of a key inside it.

Closes #59
Closes #70"

git add crates/miser-classifier/src/lib.rs crates/miser-classifier/src/kani_proofs.rs \
        crates/miser-classifier/tests/ crates/miser-classifier/Cargo.toml
run git commit -q -m "fix(classifier): panic on non-ASCII, unbounded reasoning floor, binary-question under-route

Three defects in the pure decision functions, all found by review and all
reproducible from a request body.

The \`@route:\` case-insensitive probe sliced \`first[..7]\`, a fixed byte index
with no is_char_boundary guard, in the arm that only runs when strip_prefix had
already failed. Any prompt containing a Japanese greeting panicked and
CatchPanicLayer turned it into a bare 500. Input-dependent, which is why nobody
hit it: \"@route:hard\\n<non-ascii>\" does not panic, \"日本語で...\" does.

\`coding_or_reasoning\` used word-anchored has_word for all 11 Coding keywords and
raw contains for the three Reasoning ones. \"improve\" and \"approved\" therefore
minted a Reasoning task, and miser-policy turns that into an *unbounded*
max(tier, Reasoning) -- the most expensive decision the classifier can make.

The short-definitional binary row accepted any yes/no question up to 60
characters, satisfying none of the three conditions that function documents, and
forced Trivial at the 0.95 confidence cap -- above both the 0.65 tier-floor
threshold and the 0.70 verification threshold, so neither could recover it. The
trivial tier's own binary pattern (worth 5) then decided it alone 5-to-0. The
discriminator is now whether the question asks for a fact or requests work.

The cascade's URL resolution is folded into one function: it and jev() had
separate copies, and the cascade's did not tolerate a missing leading slash, so
verification silently never happened for a class of configs. Hybrid mode no
longer returns before the cascade that wraps the dispatch.

Closes #55
Closes #64
Closes #65"

git add crates/miser-gateway/src/semantic_cache.rs
run git commit -q -m "fix(gateway): semantic cache panicked at 0 capacity, crossed tenants, ignored generation params

Three defects in the process-global semantic cache.

At max_entries = 0 -- which LLD documents as *the* way to switch the cache off --
the eviction branch called Vec::remove(0) on an empty vector. The panic
poisoned the lock, so every later store was a silent no-op and every lookup
missed: the cache was dead for the life of the process rather than erroring
once.

Entries carried no tenant, and similarity is a cosine over message text only, so
two tenants asking nearly the same question matched while their system prompts --
the part that says whose data the answer is about -- were outvoted by the shared
question. Measured at 0.96 similarity, with the second request never reaching the
provider. Entries now carry a tenant and lookup filters on it. The exact cache
needs no partition: its key hashes the whole request body.

request_text_for_embedding hashed only messages[].content, so max_tokens,
temperature, top_p and stop were invisible. A request capped at 8 tokens was
served the cached body of an identical one allowed 4096, including the first
request's usage and model fields. The in-code claim that the no-judge fallback
mirrors the exact-match safety bar was false.

Closes #56
Closes #58
Closes #61"

git add crates/miser-gateway/src/cache.rs crates/miser-gateway/src/main.rs
run git commit -q -m "fix(gateway): a cache hit bypassed the per-key tier allowlist; streaming usage was never read

The handler ran auth -> rate limit -> budget -> CACHE -> classify -> route ->
TIER GATE. The quota checks had been hoisted above the cache deliberately;
allowed_tiers had not, and a cache hit returns before classification, so the
gate never ran. A key restricted to hard was served a cached trivial body,
including one cached by a different tenant.

Both cache entry types now carry the tier that produced them, and a single
tier_permitted predicate gates all three enforcement points, so a fourth path
cannot forget it. A disallowed tier is treated as a miss rather than a 403, so a
restricted key can still be served by a tier it may use.

Streaming relayed the SSE body unparsed, so the provider's final usage frame was
never read: the ledger recorded prompt_tokens 0 and the max_tokens estimate as
completion tokens, and charged the budget the same fiction. On the shipped hard
tier (max_tokens 8192) a 200-token answer was reported as 8192 -- 41x -- and the
cap was consumed at that rate. MeteredStream now finalises accounting from the
real frame; the budget is still reserved up front so the cap holds before the
answer exists, and adjust_spend reconciles the reservation against the window it
was reserved in, floored at zero.

Also: a quality-gate escalation that died at the transport level had no else
branch, so the model the gateway escalated to could never accumulate the failures
failover_threshold needs. And cache hits were attributed to the client-requested
model and to tier \"-\", splitting rollups with a phantom bucket.

Closes #57
Closes #60
Closes #68
Closes #73"

git add crates/miser-gateway/src/auth.rs crates/miser-gateway/src/catalog.rs crates/miser-gateway/src/validate.rs crates/miser-gateway/Cargo.toml config/miser.toml
run git commit -q -m "fix(gateway): calendar budget window, inverted tier ladder, unreachable price overflow

monthly_budget_usd used secs / 2_629_800 -- 30 days 10.5 hours. Each window
started 10.5h after the last, so boundaries fell inside calendar months: 21.5h
of January 2026 belonged to the window holding most of March, and January 2027
contains two boundaries. A \$100 cap could be spent \$200 inside one calendar
month. Replaced with a real UTC calendar month via Hinnant's civil_from_days.

The price band ladder was inverted at the top: band_tier tested reasoning_max
before the Hard fallthrough, so Hard was \"above everything\" and \$1.40 banded
Reasoning while \$2.00 banded Hard. Added hard_max, so the assignment is monotone
in price. Nine catalog tests encoded the old ladder and were updated.

The price finiteness check ran on the per-token value, but the x1e6 scaling can
itself overflow: f64::MIN * 1e6 is -inf, and then every band comparison is false
while price <= 0.0 is true -- so the model read as free and was pinned as the
cheapest thing in the catalog. Scaling and the finiteness check now happen
together. (This one was introduced by the previous fix; found by
an_accepted_price_is_finite.)

A non-finite band bound passed all four order checks because every comparison is
false against NaN, and TOML accepts a nan literal, so the gateway booted with the
tier split collapsed upward. An unreadable price (null, object, bool, non-numeric
string) became \$0.00/M and, with allow_free, made a paid model the cheapest.
MISER_CATALOG_FILE was a fallback rather than the documented override, so it was
inert in the shipped deployment.

Closes #62
Closes #66
Closes #67
Closes #69
Closes #71
Closes #72
Closes #74
Closes #76"

git add crates/miser-gateway/src/judge.rs crates/miser-gateway/src/session.rs \
        crates/miser-gateway/src/properties.rs crates/miser-gateway/src/properties_support.rs \
        crates/miser-gateway/proptest-regressions/
run git commit -q -m "test(gateway): property tests for the money path and quota parsing

Eight generated properties over the code with the highest blast radius, plus a
generated one over patch_field. They are the reason two of the fixes in this
series were caught: the inverted band ladder and the -inf price.

The quota property states the parser's three legal outcomes -- absent, null,
well-typed -- and asserts a present field is always one of them or an error. The
fourth outcome that used to exist, Ok(None) for a present-but-wrong-typed value,
means *unlimited* rather than *unset* for rate_limit_rpm and monthly_budget_usd.

The calendar properties are checked against an independently written reference
calendar rather than against the implementation, so agreement is evidence rather
than a tautology. P2c (window(t) <= window(t + 31 days)) is the one the old
divisor violated, by skipping February in a non-leap year.

Closes #63"

git add .cargo/mutants.toml .gitignore
run git commit -q -m "ci: mutation testing configuration, and gitignore its scratch output

cargo test passing says the assertions agree with the code, not that they are
capable of failing. The 289 tests this was written against had no measurement of
that at all, so the kill rate is now measured on a schedule over a chosen subset
of the decision core, with --min-mutants so a shrinking list cannot report a
flattering percentage from a handful of samples.

The workflow that runs it is a separate commit, because GitHub refuses to
accept a new file under .github/workflows from a token without the workflow
scope, and splitting it keeps that from blocking the rest of the audit."

git add .github/workflows/verify.yml
run git commit -q -m "ci: tiered verification -- fast gates on PRs, proofs nightly

Kani and TLC are nightly rather than PR-blocking: they are slow and occasionally
time out, and a red build on a timeout reads as \"the prover is broken\" rather
than \"the property is violated\". A violated property is escalated loudly by the
job; only timeouts are silent.

Adds the live-contract job so the two #[ignore]d tests that need JEV_API_KEY run
weekly, gated on the secret so a fork still gets a green run."

git add spec/
run git commit -q -m "spec(tla): a machine-checked model of the authorization ordering

Defect 3 was a control placed after a bypassable return. A test for that pins
one ordering, and the one you pin is the one you remembered to write, so the
model states the property as a set equality over the controls: a served body has
been subject to all four binding controls, not just the one that was wrong.

One spec, two configurations, so the same model both gates the fix and
reproduces the defect:

  CacheConsultsTierGate = TRUE   the fixed handler. No error, 25 states.
  CacheConsultsTierGate = FALSE  the defect. Violates P8_TierGateBeforeCache
                                 with the counterexample
                                   tierAllowed     = FALSE
                                   consulted       = {Auth, RateLimit, Budget}
                                   servedFromCache = TRUE

-- which is the reachable state in which a cache hit is served with the tier gate
never consulted. CI runs the second one with its result *inverted*: a pass there
means the model has stopped discriminating, which would make the first run
meaningless.

Two things worth recording. The premise is that the quota checks precede the
cache, which is what makes P8 a theorem rather than a restatement: *if* they do,
*then* the tier gate must be too. And the order is pinned as a definition rather
than enumerated -- an earlier version generated all 120 permutations, the filter
evaluated to the empty set, and TLC reported a green run that explored zero
states. A gate that explores nothing is worse than no gate, because it looks
green."

git add docs/SPEC.md docs/CORRECTNESS_FINDINGS.md docs/CORRECTNESS_PLAN.md docs/README.md
run git commit -q -m "docs: the specification this repository is verified against

Fifteen defects were found by structured review with every corpus gate green, so
the corpora measure tier choice on clean English prose and cannot see a crash, an
authorization hole, or an accounting error. docs/SPEC.md is the missing oracle:
17 properties with formal statements, the tool that checks each one, and a
section on what it does not cover.

Section 9 records verification status honestly, including what is written but
unexecuted (Kani has no aarch64 Linux build; the mutation baseline has not been
run) and the vacuous-pass trap -- an earlier TLA+ model enumerated 120 orderings,
the filter evaluated to the empty set, and TLC reported a green run that
explored zero states. A gate that explores nothing is worse than no gate, because
it looks green."

git add scripts/open_audit_issues.sh
run git commit -q -m "chore(audit): the issue generator, so findings and tracker cannot drift

Each issue carries its reproduction, the property it violates, and why the
existing tests missed it. Kept in the repository rather than run ad hoc: the
mapping from finding to issue should be reproducible, and the previous pass
demonstrated that a document and the code drift apart without one."

if [ "$DRY_RUN" = "1" ]; then
  echo
  echo "dry run; nothing committed. status:"
  git status --short
else
  echo
  git log --oneline -9
  echo
  echo "working tree:"
  git status --short
fi
