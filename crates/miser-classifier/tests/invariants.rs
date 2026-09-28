//! Formal invariants of the routing decision.
//!
//! The corpus gates in `corpus.rs` pin *outcomes* on a fixed set of prompts.
//! These pin *properties* that must hold for every input, which is the only way
//! to catch the next phrasing nobody thought to write down.
//!
//! Several of these exist because the same class of bug has now appeared twice.
//! Substring matching in place of word matching produced "capital" -> `api` and
//! "profile" -> `file`; a case-sensitive directive silently dropped `@route:Hard`;
//! tier assignment flipped between identical requests. Each of those violated a
//! property stated here, so each would now fail the build rather than a code
//! review.
//!
//! The properties are deliberately about the *decision*, not the internal
//! scoring, so they stay true if the weights are retuned.

use miser_classifier::Classifier;
use miser_types::{ChatCompletionRequest, ClassifierConfig, ClassifierMode, ComplexityTier};
use proptest::prelude::*;
use serde_json::json;
use std::sync::OnceLock;

#[path = "support/mod.rs"]
mod support;

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| tokio::runtime::Runtime::new().expect("runtime"))
        .block_on(fut)
}

fn heuristic() -> Classifier {
    let mut config: ClassifierConfig = serde_json::from_str("{}").unwrap();
    config.mode = ClassifierMode::Heuristic;
    config.confidence_threshold = 0.65;
    Classifier::new(config).expect("heuristic classifier")
}

fn req(text: &str) -> ChatCompletionRequest {
    serde_json::from_value(json!({
        "model": "auto",
        "messages": [{"role": "user", "content": text}]
    }))
    .unwrap()
}

fn req_with(text: &str, tool: Option<&str>) -> ChatCompletionRequest {
    let mut messages = json!([{"role": "user", "content": text}]);
    let mut v = json!({"model": "auto", "messages": messages});
    if let Some(name) = tool {
        messages = json!([{"role": "user", "content": text}]);
        v["messages"] = messages;
        v["tools"] = json!([{"type": "function", "function": {"name": name}}]);
    }
    serde_json::from_value(v).unwrap()
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

/// Either the ASCII vocabulary the pattern tables key on, or arbitrary Unicode.
///
/// The union matters. The table vocabulary reaches the matching paths — the
/// original generator was right about that — but every literal in it was ASCII,
/// which made the whole class of char-boundary defects structurally unreachable.
/// `arb_straddling_text` in particular places a multi-byte character at a chosen
/// byte offset, so it cannot miss the fixed-width-slice bug that shipped.
fn arb_text() -> impl Strategy<Value = String> {
    prop_oneof![
        support::arb_table_text().boxed(),
        support::arb_adversarial_text().boxed(),
        support::arb_straddling_text().boxed(),
    ]
}

proptest! {
    // 200 keeps CI quick. The same properties have been run at 1500 cases
    // (59s) to hunt for counterexamples; raise this locally when changing the
    // pattern tables, since a shallow generator hides shallow bugs.
    #![proptest_config(ProptestConfig { cases: 200, ..ProptestConfig::default() })]

    /// 1. Totality. A heuristic classification always yields a tier. There is
    ///    no input for which the request path can fail to route.
    #[test]
    fn classification_is_total(text in arb_text()) {
        let tier = block_on(async {
            let c = heuristic();
            c.classify(&req(&text)).await
        });
        prop_assert!(tier.is_ok(), "heuristic classification failed for {:?}", text);
    }

    /// 2. Confidence is always a probability, never NaN and never out of range.
    #[test]
    fn confidence_is_always_a_probability(text in arb_text()) {
        let confidence = block_on(async {
            heuristic().classify(&req(&text)).await.unwrap().confidence
        });
        prop_assert!(
            (0.0..=1.0).contains(&confidence),
            "confidence {} is not a probability for {:?}",
            confidence,
            text
        );
    }

    /// 3. Determinism. The same request must always produce the same decision.
    ///    A classifier that flips tiers between identical requests is far harder
    ///    to diagnose in production than one that is simply wrong.
    #[test]
    fn classification_is_deterministic(text in arb_text()) {
        let (a, b) = block_on(async {
            let c = heuristic();
            let first = c.classify(&req(&text)).await.unwrap();
            let mut last = first.clone();
            for _ in 0..3 {
                last = c.classify(&req(&text)).await.unwrap();
            }
            (first, last)
        });
        prop_assert_eq!(
            (a.tier, a.classifier, a.reasons.clone()),
            (b.tier, b.classifier, b.reasons.clone()),
            "classification is not deterministic for {:?}",
            text
        );
    }

    /// 4. Case invariance. Every pattern in the tables is `(?i)`, so uppercasing
    ///    the request must not change the tier. A case-sensitive table is how
    ///    `@route:Hard` came to be silently ignored.
    #[test]
    fn tier_is_case_invariant(text in arb_text()) {
        let (lower, upper) = block_on(async {
            let c = heuristic();
            (
                c.classify(&req(&text)).await.unwrap().tier,
                c.classify(&req(&text.to_uppercase())).await.unwrap().tier,
            )
        });
        prop_assert_eq!(
            lower, upper,
            "case changed the tier for {:?}: {:?} vs {:?}",
            text,
            lower,
            upper
        );
    }

    /// 5. Whitespace invariance. Padding must not change the tier. Several
    ///    patterns are anchored with `^...$`, so a stray space is exactly the
    ///    kind of thing that makes a table quietly stop matching.
    #[test]
    fn tier_is_whitespace_invariant(text in arb_text()) {
        let (bare, padded) = block_on(async {
            let c = heuristic();
            (
                c.classify(&req(&text)).await.unwrap().tier,
                c.classify(&req(&format!("  \n\t{text}  \n "))).await.unwrap().tier,
            )
        });
        prop_assert_eq!(
            bare, padded,
            "surrounding whitespace changed the tier for {:?}: {:?} vs {:?}",
            text,
            bare,
            padded
        );
    }

    /// 6. Monotonicity in context. Attaching a tool can only ever add capability
    ///    requirement, never remove it: the tool rules all add to Hard and
    ///    nothing subtracts. If this fails, a request is being made *cheaper* by
    ///    carrying more information, which is under-routing by another name.
    #[test]
    fn attaching_a_tool_never_lowers_the_tier(text in arb_text()) {
        let (bare, with_tool) = block_on(async {
            let c = heuristic();
            (
                c.classify(&req(&text)).await.unwrap().tier,
                c.classify(&req_with(&text, Some("read_file"))).await.unwrap().tier,
            )
        });
        prop_assert!(
            rank(with_tool) >= rank(bare),
            "attaching a tool lowered the tier for {:?}: {:?} -> {:?}",
            text,
            bare,
            with_tool
        );
    }
}

/// 7. Override dominance. A valid `@route:` on the user turn always wins, and is
///    always reported as an override so the caller can tell it took effect.
#[tokio::test]
async fn a_valid_route_directive_always_wins() {
    let classifier = heuristic();
    for tier in ["trivial", "simple", "standard", "hard", "reasoning"] {
        // Paired with text that would otherwise land far away from the directive,
        // so a directive that is silently dropped cannot pass by accident.
        for text in [
            "hello",
            "design a distributed multi-region migration for 200 microservices",
            "add request validation to the auth endpoints",
            "explain DNS in one sentence",
            "prove the correctness of this reduction",
        ] {
            let body = format!("@route:{tier}\n{text}");
            let result = classifier.classify(&req(&body)).await.unwrap();
            assert_eq!(
                result.tier,
                match tier {
                    "trivial" => ComplexityTier::Trivial,
                    "simple" => ComplexityTier::Simple,
                    "standard" => ComplexityTier::Standard,
                    "hard" => ComplexityTier::Hard,
                    _ => ComplexityTier::Reasoning,
                },
                "@route:{tier} was not honoured for {text:?}"
            );
            assert_eq!(result.classifier, "override", "@route:{tier} misattributed");
        }
    }
}

/// 8. Override soundness. An unrecognised tier must never be honoured: failing
///    open into the heuristic is the risky direction for an explicit request, so
///    it is warned about rather than silently obeyed.
#[tokio::test]
async fn an_unknown_route_tier_is_never_honoured() {
    let classifier = heuristic();
    for directive in ["banana", "TRIVIAL1", "", "hard-ish", "very hard"] {
        let body = format!("@route:{directive}\nadd request validation to the auth endpoints");
        let result = classifier.classify(&req(&body)).await.unwrap();
        assert_ne!(
            result.classifier, "override",
            "@route:{directive:?} was honoured as an override"
        );
    }
}

/// 9. Override case-insensitivity, since every other pattern in the file is.
#[tokio::test]
async fn route_directive_is_case_insensitive() {
    let classifier = heuristic();
    for spelling in ["@route:hard", "@ROUTE:HARD", "@Route:Hard", "@rOuTe:hArD"] {
        let result = classifier
            .classify(&req(&format!(
                "{spelling}\nadd request validation to the auth endpoints"
            )))
            .await
            .unwrap();
        assert_eq!(
            result.tier,
            ComplexityTier::Hard,
            "{spelling} was not honoured"
        );
    }
}

/// 10. The directive belongs to the user, not to whatever opens the transcript.
///     A silently ignored explicit instruction is worse than a rejected one,
///     because the caller cannot tell it did not take effect.
#[tokio::test]
async fn route_directive_survives_leading_non_user_turns() {
    let classifier = heuristic();
    for prefix in [
        json!([{"role": "system", "content": "You are helpful."}]),
        json!([{"role": "assistant", "content": null}]),
        json!([
            {"role": "system", "content": "Be terse."},
            {"role": "assistant", "content": null},
            {"role": "user", "content": "earlier question"}
        ]),
    ] {
        let request: ChatCompletionRequest = serde_json::from_value(json!({
            "model": "auto",
            "messages": [
                {"role": "user", "content": "@route:hard\ndesign a system"}
            ]
        }))
        .unwrap();
        let mut with_prefix = request.clone();
        // Splice the directive behind the prefix turns.
        let directive = json!([{"role": "user", "content": "@route:hard\ndesign a system"}]);
        let mut messages = prefix.as_array().unwrap().clone();
        messages.extend(directive.as_array().unwrap().clone());
        with_prefix.messages = serde_json::from_value(json!(messages)).unwrap();

        let result = classifier.classify(&with_prefix).await.unwrap();
        assert_eq!(
            result.tier,
            ComplexityTier::Hard,
            "override dropped behind {prefix}"
        );
    }
}

/// A short definitional question is Trivial regardless of what it names.
///
/// This is the property #51 broke, and that its first fix broke worse: the tier
/// tables are keyed on technology nouns, so `crdt` (Hard) promoted "What does
/// 'CRDT' stand for? One sentence." to the strongest model.
#[tokio::test]
async fn a_short_definitional_question_is_trivial() {
    let classifier = heuristic();
    for (prompt, label) in [
        (
            "What does 'CRDT' stand for? One sentence.",
            "hard-table noun",
        ),
        (
            "what is a bloom filter? two sentences max",
            "standard-table noun",
        ),
        (
            "Just say 'distributed consensus' in a sentence so I can quote it",
            "hard-table noun",
        ),
        ("thanks, that worked", "bare acknowledgment"),
        (
            "hello! no need to do anything with kubernetes today, just checking",
            "infra noun in small talk",
        ),
        (
            "just answer yes or no: is Python interpreted?",
            "binary question",
        ),
    ] {
        let result = classifier.classify(&req(prompt)).await.unwrap();
        assert_eq!(
            result.tier,
            ComplexityTier::Trivial,
            "{prompt:?} ({label}) got {:?} ({:?})",
            result.tier,
            result.reasons
        );
    }
}

/// The other half: the ceiling must NOT fire on real work.
///
/// The first attempt capped on any brevity marker anywhere in the text, caught
/// "explain X in one sentence", and cost 9 points of accuracy. These are the
/// cases that must keep their tier.
/// A yes/no *framing* is not a synonym for a small request. The binary row in
/// the short-definitional form used to accept any yes/no question up to 60
/// characters and force `Trivial` at the 0.95 confidence cap — which is above
/// both the 0.65 tier-floor threshold and the 0.70 verification threshold, so
/// neither the policy floor nor the verification cascade could recover it. These
/// are binary questions about real work, and they must not be capped.
#[tokio::test]
async fn a_binary_question_about_real_work_is_not_capped_as_trivial() {
    let classifier = heuristic();
    for prompt in [
        "true or false: delete every row in prod and rebuild the index?",
        "yes or no: rewrite the payment service in Rust?",
        "yes or no: should we migrate the ledger to event sourcing?",
    ] {
        let result = classifier.classify(&req(prompt)).await.unwrap();
        assert_ne!(
            result.tier,
            ComplexityTier::Trivial,
            "{prompt:?} was capped to Trivial at confidence {} ({:?})",
            result.confidence,
            result.reasons
        );
    }
}

/// The control: a binary question that asks for a *fact* is still trivial. The
/// corpus pins "just answer yes or no: is Python interpreted?" at Trivial, so
/// closing the hole above must not have closed this one with it.
#[tokio::test]
async fn a_binary_question_about_a_fact_is_still_trivial() {
    let classifier = heuristic();
    for prompt in [
        "just answer yes or no: is Python interpreted?",
        "yes or no: is the capital of France Paris?",
    ] {
        let result = classifier.classify(&req(prompt)).await.unwrap();
        assert_eq!(
            result.tier,
            ComplexityTier::Trivial,
            "{prompt:?} is a closed-form factual question ({:?})",
            result.reasons
        );
    }
}

#[tokio::test]
async fn real_work_is_not_capped_as_definitional() {
    let classifier = heuristic();
    for (prompt, at_least) in [
        (
            "Explain DNS resolution in one sentence",
            ComplexityTier::Simple,
        ),
        (
            "What does the CRDT merge protocol guarantee about convergence under partition?",
            ComplexityTier::Simple,
        ),
    ] {
        let result = classifier.classify(&req(prompt)).await.unwrap();
        assert!(
            rank(result.tier) >= rank(at_least),
            "{prompt:?} was capped to {:?}, below {at_least:?} ({:?})",
            result.tier,
            result.reasons
        );
    }
}
