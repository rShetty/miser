//! The tier floors: `miser-policy` decides which tier a request may use, and
//! therefore what it may spend. These tests are about that decision.
//!
//! ## Why these exist
//!
//! Mutation testing reported 23 surviving mutants in this crate, and the cause
//! was not an absence of tests -- `lib.rs` already had three. It was that those
//! three do not *discriminate*. Every one of them uses `confidence: 0.99`, so
//! the confidence gate
//!
//! ```text
//! if classification.confidence < self.config.classifier.confidence_threshold
//! ```
//!
//! never runs on the interesting side. Mutating `<` to `<=` or to `>` left the
//! suite green. A test that cannot tell the original from a broken version is
//! not evidence, which is the same claim the mutation run makes about the rest
//! of the workspace.
//!
//! ## The property that matters
//!
//! Every branch of `effective_tier` is a `max_tier` promotion. A floor, never a
//! ceiling. So the function is monotone in each of its five inputs, and that is
//! what these assert:
//!
//! - raising `confidence` never raises the tier (uncertainty must cost more)
//! - adding `tools`, `response_format`, a tool history, or a harder task never
//!   lowers the tier
//!
//! Monotonicity is the real safety statement here. It says the gateway cannot be
//! talked *down* into a cheaper model by any combination of request fields.

use miser_policy::{PolicyEngine, has_tool_history};
use miser_types::{
    ChatCompletionRequest, ClassificationResult, ComplexityTier, GatewayConfig, TaskType,
};
use proptest::prelude::*;
use serde_json::json;

/// The shipped configuration. The floors are only meaningful against real
/// thresholds, and a test that invented its own would pass while the deployed
/// config did not.
fn config() -> GatewayConfig {
    toml::from_str(include_str!("../../../config/miser.toml")).expect("config/miser.toml parses")
}

fn policy() -> PolicyEngine {
    PolicyEngine::new(config())
}

/// The threshold the shipped config actually uses. Hard-coded here on purpose:
/// if a config change moves it, a test that read it back out of the config would
/// silently follow and keep passing, which is exactly the failure this file
/// exists to catch.
const THRESHOLD: f32 = 0.65;

fn classification(tier: ComplexityTier, confidence: f32) -> ClassificationResult {
    ClassificationResult {
        tier,
        confidence,
        reasons: vec![],
        classifier: "test".into(),
        latency_ms: 0,
        ..Default::default()
    }
}

fn request(value: serde_json::Value) -> ChatCompletionRequest {
    serde_json::from_value(value).expect("request shape is constructible")
}

fn plain() -> serde_json::Value {
    json!({"model": "auto", "messages": [{"role": "user", "content": "hello"}]})
}

/// Every tier, as a strategy.
///
/// Written out rather than `any::<ComplexityTier>()`: `Arbitrary` is not
/// implemented on the type, and adding it would put a proptest dependency into
/// `miser-types` for the convenience of one test. An `Arbitrary` derive on a
/// public enum is also a silent contract -- it says the enum's variants can
/// grow, and a strategy derived from the variant list would quietly start
/// generating a new tier before any floor is defined for it. Enumerating the
/// five here means a sixth tier fails this file to compile.
fn any_tier() -> impl Strategy<Value = ComplexityTier> {
    prop_oneof![
        Just(ComplexityTier::Trivial),
        Just(ComplexityTier::Simple),
        Just(ComplexityTier::Standard),
        Just(ComplexityTier::Hard),
        Just(ComplexityTier::Reasoning),
    ]
}

fn rank(tier: ComplexityTier) -> u8 {
    match tier {
        ComplexityTier::Trivial => 0,
        ComplexityTier::Simple => 1,
        ComplexityTier::Standard => 2,
        ComplexityTier::Hard => 3,
        ComplexityTier::Reasoning => 4,
    }
}

// ---------------------------------------------------------------- the floors

/// The confidence gate. Below the threshold the classification is not trusted
/// and the request is promoted to at least Standard -- this is the test that
/// distinguishes `<` from `<=`, which no existing test did.
#[test]
fn low_confidence_is_promoted_to_at_least_standard() {
    let engine = policy();
    for tier in [
        ComplexityTier::Trivial,
        ComplexityTier::Simple,
        ComplexityTier::Standard,
    ] {
        let low = classification(tier, THRESHOLD - 0.01);
        let got = engine.effective_tier(&request(plain()), &low);
        assert!(
            rank(got) >= rank(ComplexityTier::Standard),
            "confidence {} below threshold should floor {tier:?} at Standard, got {got:?}",
            low.confidence
        );
    }
}

/// The other side: at or above the threshold the classifier's own tier stands.
/// Together with the test above this pins the comparison to exactly `<` --
/// `<=` would promote at the threshold, `>` would promote above it.
#[test]
fn confidence_at_or_above_the_threshold_keeps_the_classified_tier() {
    let engine = policy();
    for confidence in [THRESHOLD, THRESHOLD + 0.01, 0.99] {
        let c = classification(ComplexityTier::Trivial, confidence);
        assert_eq!(
            engine.effective_tier(&request(plain()), &c),
            ComplexityTier::Trivial,
            "confidence {confidence} at or above the threshold should not promote"
        );
    }
}

/// `has_tool_history` is a three-way disjunction. The existing test used a
/// message that satisfies all three at once, so replacing any single `||` with
/// `&&` still returned true. Each disjunct is checked on its own.
#[test]
fn each_tool_history_disjunct_is_independently_sufficient() {
    // tool_calls present, nothing else
    assert!(has_tool_history(&request(json!({
        "model": "auto",
        "messages": [{"role": "assistant", "content": "ok", "tool_calls": [{"id": "1"}]}]
    }))));
    // tool_call_id present, nothing else
    assert!(has_tool_history(&request(json!({
        "model": "auto",
        "messages": [{"role": "tool", "content": "result", "tool_call_id": "1"}]
    }))));
    // role == "tool" only
    assert!(has_tool_history(&request(json!({
        "model": "auto",
        "messages": [{"role": "tool", "content": "result"}]
    }))));
}

/// ...and a transcript with none of them is not tool history. Without this,
/// replacing the whole disjunction with a constant `true` would survive.
#[test]
fn a_plain_transcript_is_not_tool_history() {
    assert!(!has_tool_history(&request(json!({
        "model": "auto",
        "messages": [
            {"role": "user", "content": "hello"},
            {"role": "assistant", "content": "hi"}
        ]
    }))));
    assert!(!has_tool_history(&request(
        json!({"model": "auto", "messages": []})
    )));
}

/// `select` must fail closed. If the effective tier has no route, the answer is
/// an error, never a silent downgrade to something cheaper.
#[test]
fn a_tier_with_no_configured_route_is_an_error_not_a_downgrade() {
    let mut config = config();
    config.tiers.remove(&ComplexityTier::Hard);
    let engine = PolicyEngine::new(config);

    let request = request(json!({
        "model": "auto",
        "messages": [{"role": "user", "content": "hello"}]
    }));
    let mut c = classification(ComplexityTier::Trivial, 0.99);
    c.task = Some(TaskType::Agentic); // forces Hard

    let err = engine.select(&request, &c);
    assert!(
        err.is_err(),
        "a request whose floor has no route must error, not be served cheaper"
    );
}

/// Escalation stops at the top of the ladder rather than wrapping or returning
/// the same tier again, which would make the escalation loop non-terminating.
#[test]
fn escalation_stops_at_the_top_tier() {
    let engine = policy();
    let request = request(plain());
    let c = classification(ComplexityTier::Reasoning, 0.99);
    assert_eq!(
        engine.escalated_tier(&request, &c),
        None,
        "there is no tier above Reasoning"
    );
}

// ------------------------------------------------------------- monotonicity

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    /// Raising confidence must never raise the tier. This is the safety
    /// direction: less certainty is allowed to cost more, never less.
    #[test]
    fn raising_confidence_never_raises_the_tier(
        base_tier in any_tier(),
        low in 0.0f32..THRESHOLD,
        high in THRESHOLD..1.0f32,
    ) {
        let engine = policy();
        let req = request(plain());
        let at_low = engine.effective_tier(&req, &classification(base_tier, low));
        let at_high = engine.effective_tier(&req, &classification(base_tier, high));
        prop_assert!(
            rank(at_low) >= rank(at_high),
            "confidence {low} gave {at_low:?} but higher confidence {high} gave {at_high:?}"
        );
    }

    /// The confidence gate never *lowers* a tier, at any confidence. A floor
    /// that could demote would make the cheap path reachable by being unsure.
    #[test]
    fn confidence_never_demotes_below_the_classified_tier(
        tier in any_tier(),
        confidence in 0.0f32..=1.0f32,
    ) {
        let engine = policy();
        let got = engine.effective_tier(&request(plain()), &classification(tier, confidence));
        prop_assert!(
            rank(got) >= rank(tier),
            "confidence {confidence} demoted {tier:?} to {got:?}"
        );
    }

    /// Declaring tools can only raise the floor.
    #[test]
    fn declaring_tools_never_lowers_the_tier(tier in any_tier(), confidence in 0.0f32..=1.0f32) {
        let engine = policy();
        let c = classification(tier, confidence);
        let bare = engine.effective_tier(&request(plain()), &c);
        let with_tools = engine.effective_tier(
            &request(json!({
                "model": "auto",
                "messages": [{"role": "user", "content": "hello"}],
                "tools": [{"type": "function"}]
            })),
            &c,
        );
        prop_assert!(
            rank(with_tools) >= rank(bare),
            "tools lowered {bare:?} to {with_tools:?}"
        );
    }

    /// A tool-call transcript can only raise the floor.
    #[test]
    fn tool_history_never_lowers_the_tier(tier in any_tier(), confidence in 0.0f32..=1.0f32) {
        let engine = policy();
        let c = classification(tier, confidence);
        let bare = engine.effective_tier(&request(plain()), &c);
        let replayed = engine.effective_tier(
            &request(json!({
                "model": "auto",
                "messages": [
                    {"role": "user", "content": "run it"},
                    {"role": "assistant", "content": "ok", "tool_calls": [{"id": "1"}]}
                ]
            })),
            &c,
        );
        prop_assert!(
            rank(replayed) >= rank(bare),
            "tool history lowered {bare:?} to {replayed:?}"
        );
    }

    /// `response_format` is a structured-output request; it can only raise the
    /// floor.
    #[test]
    fn response_format_never_lowers_the_tier(tier in any_tier(), confidence in 0.0f32..=1.0f32) {
        let engine = policy();
        let c = classification(tier, confidence);
        let bare = engine.effective_tier(&request(plain()), &c);
        let structured = engine.effective_tier(
            &request(json!({
                "model": "auto",
                "messages": [{"role": "user", "content": "hello"}],
                "response_format": {"type": "json_object"}
            })),
            &c,
        );
        prop_assert!(
            rank(structured) >= rank(bare),
            "response_format lowered {bare:?} to {structured:?}"
        );
    }
}

/// An agentic task must reach at least Hard, whatever else is true of the
/// request. This is the strongest single floor and the easiest to break
/// accidentally.
#[test]
fn an_agentic_task_always_reaches_at_least_hard() {
    let engine = policy();
    for tier in [
        ComplexityTier::Trivial,
        ComplexityTier::Simple,
        ComplexityTier::Standard,
        ComplexityTier::Hard,
    ] {
        for confidence in [0.0, 0.5, THRESHOLD, 0.99] {
            let mut c = classification(tier, confidence);
            c.task = Some(TaskType::Agentic);
            let got = engine.effective_tier(&request(plain()), &c);
            assert!(
                rank(got) >= rank(ComplexityTier::Hard),
                "agentic {tier:?} at confidence {confidence} gave {got:?}, below Hard"
            );
        }
    }
}

/// A reasoning task must reach the Reasoning tier -- the top of the ladder, and
/// the most expensive. An `improve`-style false positive here is the single
/// most expensive mistake the policy can make.
#[test]
fn a_reasoning_task_reaches_the_reasoning_tier() {
    let engine = policy();
    for tier in [
        ComplexityTier::Trivial,
        ComplexityTier::Simple,
        ComplexityTier::Standard,
    ] {
        let mut c = classification(tier, 0.99);
        c.task = Some(TaskType::Reasoning);
        assert_eq!(
            engine.effective_tier(&request(plain()), &c),
            ComplexityTier::Reasoning,
            "reasoning task from {tier:?} should reach Reasoning"
        );
    }
}

/// Escalation actually escalates.
///
/// The companion to `escalation_stops_at_the_top_tier`, and the test whose
/// absence let three mutants survive the previous commit. A test that only
/// checks the ceiling cannot tell "there is no tier above Reasoning" from
/// "escalation is silently broken" -- and those are the same mutant from the
/// suite's point of view.
#[test]
fn escalation_raises_the_tier_by_one() {
    let engine = policy();
    let req = request(plain());
    for (from, to) in [
        (ComplexityTier::Trivial, ComplexityTier::Simple),
        (ComplexityTier::Simple, ComplexityTier::Standard),
        (ComplexityTier::Standard, ComplexityTier::Hard),
        (ComplexityTier::Hard, ComplexityTier::Reasoning),
    ] {
        assert_eq!(
            engine.escalated_tier(&req, &classification(from, 0.99)),
            Some(to),
            "{from:?} should escalate to {to:?}"
        );
    }
}

/// `next()` returns the route configured for the *escalated* tier.
///
/// Two distinct mutants die here. `next()` returning `Ok(None)` for a tier that
/// does have a parent disables quality escalation entirely; and `next()`
/// returning the current tier's route makes escalation a no-op that still costs
/// a second upstream call.
///
/// Compared against what the config says the parent tier routes to, not against
/// "a different model from the current one". Two tiers are *allowed* to share a
/// model, and asserting they differ would test the config rather than the code.
/// Whether sharing a model is a good idea is a separate question -- see the
/// duplicate-route finding for `[tiers.hard]` and `[tiers.reasoning]`, which
/// both route to `z-ai/glm-5.3` and so make escalation from Hard a paid no-op.
#[test]
fn next_returns_the_configured_route_for_the_escalated_tier() {
    let config = config();
    let engine = PolicyEngine::new(config.clone());
    let req = request(plain());

    for from in [
        ComplexityTier::Trivial,
        ComplexityTier::Simple,
        ComplexityTier::Standard,
        ComplexityTier::Hard,
    ] {
        let parent = match from {
            ComplexityTier::Trivial => ComplexityTier::Simple,
            ComplexityTier::Simple => ComplexityTier::Standard,
            ComplexityTier::Standard => ComplexityTier::Hard,
            ComplexityTier::Hard => ComplexityTier::Reasoning,
            ComplexityTier::Reasoning => continue,
        };
        let expected = config
            .tiers
            .get(&parent)
            .expect("the shipped config defines every tier")
            .model
            .clone();

        let c = classification(from, 0.99);
        let escalated = engine
            .next(&req, &c)
            .expect("no error")
            .expect("a tier above exists, so next() must not return None");

        assert_eq!(
            escalated.model, expected,
            "next() from {from:?} must return the route configured for {parent:?}"
        );
    }
}

/// Escalation must not be a no-op: a parent tier that routes to the *same*
/// model spends a second upstream call for an identical answer.
///
/// Currently **true is not the case**: `[tiers.hard]` and `[tiers.reasoning]` both
/// route to `z-ai/glm-5.3`, so every quality escalation out of the Hard tier is a
/// full-price call that buys nothing (issue #84).
///
/// This asserts the *known* duplicate set rather than asserting there are no
/// duplicates, so it is a characterization test:
///
/// - it passes today, with the defect written into the expectation, so the suite
///   is green and the cost is visible in the diff;
/// - it fails the moment someone gives Reasoning a genuinely stronger model,
///   telling them to update the expectation rather than silently deleting the
///   check.
///
/// Deliberately NOT `#[ignore]`d. In this repository `#[ignore]` means "needs a
/// live provider key" -- `.github/workflows/verify.yml` runs
/// `cargo test --workspace -- --ignored` in the live-contract job, so an ignored
/// red-by-design test breaks that gate. A test that is meant to fail has to
/// encode the current behaviour instead, or it will be run by the one job whose
/// whole purpose is to run exactly those tests.
#[test]
fn escalation_into_a_duplicate_route_is_known_and_tracked() {
    let config = config();
    let pairs = [
        (ComplexityTier::Trivial, ComplexityTier::Simple),
        (ComplexityTier::Simple, ComplexityTier::Standard),
        (ComplexityTier::Standard, ComplexityTier::Hard),
        (ComplexityTier::Hard, ComplexityTier::Reasoning),
    ];

    let mut duplicates = Vec::new();
    for (child, parent) in pairs {
        // A tier absent from the config has no route to duplicate, so a missing
        // one is not a finding.
        let (Some(child_route), Some(parent_route)) =
            (config.tiers.get(&child), config.tiers.get(&parent))
        else {
            continue;
        };
        if child_route.model == parent_route.model {
            duplicates.push((child, parent, child_route.model.clone()));
        }
    }

    let actual: Vec<(ComplexityTier, ComplexityTier, String)> = duplicates;

    let known: Vec<(ComplexityTier, ComplexityTier, &str)> = vec![(
        ComplexityTier::Hard,
        ComplexityTier::Reasoning,
        "z-ai/glm-5.3",
    )];

    let actual_display: Vec<(ComplexityTier, ComplexityTier, &str)> = actual
        .iter()
        .map(|(c, p, m)| (*c, *p, m.as_str()))
        .collect();

    assert_eq!(
        actual_display, known,
        "the set of duplicate tier routes changed.\n\
         If Reasoning now has a genuinely stronger model, this is the fix for \
         #84 -- delete the entry from `known` and remove the test if there are no \
         duplicates left. If a new pair has started sharing a model, that is a new \
         paid no-op and #84 should be reopened."
    );
}

// ------------------------------------------------------------ quality gate
//
// `deterministic_quality` had 7 surviving mutants, all `||` -> `&&` in one
// function. Same non-discriminating pattern as the tier floors: the existing
// test `coding_output_needs_code_markers_or_substance` uses content containing a
// fence, so all seven operands in the `has_code` chain read true and replacing
// any single `||` with `&&` changed nothing observable.
//
// Seven operands means the gate is *or*-shaped: any one marker is enough. That
// is the property worth asserting, and it is only checkable by giving each
// operand a case where it is the only true term.

use miser_policy::quality::{QualityScore, deterministic_quality};
use miser_types::QualityConfig;

/// A response body with `content` set, as the quality gate reads it.
fn response_with(content: &str) -> serde_json::Value {
    json!({"choices": [{"message": {"role": "assistant", "content": content}}]})
}

fn quality_config() -> QualityConfig {
    // Annotated: `.quality` alone leaves `from_str`'s type parameter ambiguous.
    let full: GatewayConfig = toml::from_str(include_str!("../../../config/miser.toml"))
        .expect("config/miser.toml parses");
    full.quality
}

fn score_of(content: &str, task: Option<TaskType>) -> QualityScore {
    let req = request(plain());
    let mut c = classification(ComplexityTier::Trivial, 0.99);
    c.task = task;
    deterministic_quality(&req, &response_with(content), &c, &quality_config())
}

/// Below 80 chars, inside the Coding/Agentic branch: without a code marker the
/// gate returns `insufficient-output`, with one it does not. That difference is
/// the only observable, so each marker is tested as the sole true operand.
const SHORT_PROSE: &str = "here is a short answer with no code in it at all";

/// Two of the seven `has_code` operands are *not* in the list below, and their
/// mutants cannot be killed:
///
/// - `content.contains("```shell")` and `content.contains("```bash")` are
///   subsumed by `content.contains("```")`, which is already an operand.
///   Anything containing "```shell" necessarily contains "```", so the two add
///   nothing and removing them is unobservable.
/// - The fence disjunct on the *outer* condition
///   (`task == Coding || task == Agentic || content.contains("```")`) is
///   unobservable for the same reason: entering that branch with a fence makes
///   `has_code` true, so nothing is rejected, and not entering it scores by
///   length either way.
///
/// These are left in the source deliberately. Removing them is provably
/// behaviour-preserving *today*, but it couples the operands to "```" remaining
/// in the chain: drop that one later and the shell/bash checks silently become
/// live again. Kept, and documented, rather than quietly deleted. Tracked in
/// issue #85.
#[test]
fn each_code_marker_alone_satisfies_the_has_code_gate() {
    // Every marker, on its own, must be enough. The whole chain is an `or`, so
    // removing any single `||` makes exactly one of these fail.
    for marker in [
        "```",
        "fn ",
        "function ",
        "def ",
        "tool_call",
        "```shell",
        "```bash",
    ] {
        let content = format!("x {marker} y");
        assert!(
            content.len() < 80,
            "test case must stay under the 80-char gate: {content:?}"
        );
        let got = score_of(&content, Some(TaskType::Coding));
        assert_ne!(
            got.reason, "insufficient-output",
            "marker {marker:?} alone should satisfy the has_code gate, got {got:?}"
        );
    }
}

/// The control: with no marker at all, the gate must reject. Without this, the
/// test above would also pass if the gate were simply always-accept.
#[test]
fn no_code_marker_at_all_is_insufficient_output() {
    let got = score_of(SHORT_PROSE, Some(TaskType::Coding));
    assert_eq!(
        got.reason, "insufficient-output",
        "prose with no code marker should fail the gate, got {got:?}"
    );
}

/// The same disjunction one level up: `task == Coding`, `task == Agentic`, or a
/// fence in the content. Each must independently reach the has_code gate.
#[test]
fn each_branch_of_the_task_or_fence_disjunction_reaches_the_gate() {
    // Coding via the task
    assert_eq!(
        score_of(SHORT_PROSE, Some(TaskType::Coding)).reason,
        "insufficient-output",
        "a Coding task with no marker should reach the gate and fail it"
    );
    // Agentic via the task
    assert_eq!(
        score_of(SHORT_PROSE, Some(TaskType::Agentic)).reason,
        "insufficient-output",
        "an Agentic task with no marker should reach the gate and fail it"
    );
    // Chat task, but a fence in the content.
    //
    // This is where the 57:9 mutant (`||` -> `&&` on the fence disjunct) lives,
    // and it cannot be killed by any test -- correctly so. The fence disjunct is
    // unobservable: entering the branch with a fence sets `has_code = true`, so
    // `!has_code` is false and nothing is rejected, and *not* entering the
    // branch falls through to the same length-based score. Both paths give
    // "deterministic-content-check". Asserting anything else here would be
    // asserting the code does something it does not.
    assert_eq!(
        score_of("```\nx\n```", Some(TaskType::Chat)).reason,
        "deterministic-content-check",
        "a fence sets has_code, so the gate passes and scoring falls through to length"
    );
    // Chat task, no fence, no marker: the branch is not entered at all, so the
    // gate never runs and the answer is scored by length instead.
    assert_eq!(
        score_of(SHORT_PROSE, Some(TaskType::Chat)).reason,
        "deterministic-content-check",
        "with no task and no fence the branch is skipped entirely"
    );
}
