//! The classifier's own tool-history detector.
//!
//! # Why this file exists separately from `miser-policy`'s tests
//!
//! `has_tool_history` is defined **twice** — once in `miser-classifier` and once
//! in `miser-policy` — with identical bodies and no shared code. Mutation testing
//! reported two surviving `||` -> `&&` mutants in this copy while the policy copy
//! was covered. Duplicated logic gets covered unevenly, which is part of the
//! argument for `miser-core` (#78); until then each copy needs its own test.
//!
//! # The trap in writing this test
//!
//! The first version of these tests used the prompt "run the suite" and asserted
//! the tier reached Hard. It passed — and killed nothing. "run the suite" is
//! *already* a Hard keyword in the heuristic, so the tool-history signal
//! contributed nothing and mutating `has_tool_history` to `&&` left every
//! assertion green.
//!
//! A test for a detector has to make the detector the *only* thing carrying the
//! outcome. The prompts here are deliberately inert: a bare "hello" classifies as
//! Trivial, and each tool-history operand alone is what moves it to Hard. The
//! measured table, which is what the tests assert:
//!
//! ```text
//! prompt      bare     tool_calls  tool_call_id  role=tool
//! "hello"     Trivial  Hard        Hard          Hard
//! "thanks"    Trivial  Hard        Hard          Hard
//! ```
//!
//! `has_multi_step_intent` is deliberately not tested here. It is private, and
//! the tier it feeds is not a function of it: "read the config and update the
//! port" matches the multi-step regex and classifies as Standard, while "run and
//! report" does not match it and classifies as Hard, because the heuristic has
//! its own keyword tiers. Asserting on the tier would test that keyword soup. It
//! is covered as a unit test inside `lib.rs`, where the function is reachable.

use miser_classifier::Classifier;
use miser_types::{ChatCompletionRequest, ClassifierConfig, ClassifierMode, ComplexityTier};
use serde_json::json;
use std::sync::OnceLock;

/// Inert prompts: no keyword that the heuristic would route to Hard on its own.
const INERT: [&str; 2] = ["hello", "thanks"];

fn heuristic() -> Classifier {
    let mut config: ClassifierConfig = serde_json::from_str("{}").expect("defaults parse");
    config.mode = ClassifierMode::Heuristic;
    Classifier::new(config).expect("heuristic classifier constructs")
}

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| tokio::runtime::Runtime::new().expect("runtime"))
        .block_on(fut)
}

fn tier_of(value: serde_json::Value) -> ComplexityTier {
    let req: ChatCompletionRequest = serde_json::from_value(value).expect("request shape");
    block_on(heuristic().classify(&req))
        .unwrap_or_else(|e| panic!("failed to classify: {e}"))
        .tier
}

/// A transcript built from an inert prompt plus exactly one tool-history marker.
fn with_marker(prompt: &str, marker: &str) -> serde_json::Value {
    let second = match marker {
        "tool_calls" => json!({"role": "assistant", "content": "ok", "tool_calls": [{"id": "c1"}]}),
        "tool_call_id" => json!({"role": "assistant", "content": "ok", "tool_call_id": "c1"}),
        "role" => json!({"role": "tool", "content": "ok"}),
        other => panic!("unknown marker {other:?}"),
    };
    json!({
        "model": "auto",
        "messages": [
            {"role": "user", "content": prompt},
            second
        ]
    })
}

/// Each of the three operands is sufficient on its own, and the other two are
/// absent. This is what makes an `||` -> `&&` mutation observable: any single
/// operator change breaks exactly one of these cases.
#[test]
fn each_tool_history_operand_alone_reaches_hard() {
    for prompt in INERT {
        for marker in ["tool_calls", "tool_call_id", "role"] {
            let got = tier_of(with_marker(prompt, marker));
            assert!(
                got >= ComplexityTier::Hard,
                "{prompt:?} with only {marker:?} should reach Hard, got {got:?}"
            );
        }
    }
}

/// The control, and the reason the test above discriminates: an inert prompt
/// with no tool history must stay below Hard. Without this, the assertions above
/// would also pass if the detector were always-true — and, more to the point,
/// if the prompt were independently a Hard keyword.
#[test]
fn an_inert_prompt_with_no_tool_history_stays_below_hard() {
    for prompt in INERT {
        let got = tier_of(json!({
            "model": "auto",
            "messages": [{"role": "user", "content": prompt}]
        }));
        assert!(
            got < ComplexityTier::Hard,
            "{prompt:?} has no tool history and must not reach Hard, got {got:?}"
        );
    }
}
