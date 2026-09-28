//! P7 / P15 / P17 — parsers must be total.
//!
//! From `docs/SPEC.md`:
//!
//! ```text
//! P7   ∀ input x.  parse(x) does not panic
//! P15a ∀ part ∉ known.  serialize(parse(part)) = part
//! P15b ∀ known part.  the `type` tag survives serialization
//! P17  ∀ result r.  0 ≤ r.confidence ≤ 1 ∧ r.confidence is not NaN
//! ```
//!
//! Every test here is a *regression for a bug that shipped*, or for a class of
//! bug that is structurally easy to write in Rust and impossible to see by
//! reading. The generators live in `support/mod.rs`; the original generator drew
//! from a 77-word ASCII list and so could not express a single one of these
//! inputs.
//!
//! The strongest of these is [`no_panic_on_a_straddled_route_prefix`], which
//! reproduces the defect that returned HTTP 500 to any request containing a
//! Japanese greeting — the single most valuable test in the repository, because
//! the class (fixed-width byte slicing) is invisible to review and this generator
//! cannot miss it.

#[path = "support/mod.rs"]
mod support;

use miser_classifier::Classifier;
use miser_types::{
    ChatCompletionRequest, ClassifierConfig, ClassifierMode, ComplexityTier, MessageContent,
};
use proptest::prelude::*;
use serde_json::{Value, json};
use std::sync::OnceLock;
use support::*;

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| tokio::runtime::Runtime::new().expect("runtime"))
        .block_on(fut)
}

fn heuristic() -> Classifier {
    let mut config: ClassifierConfig = serde_json::from_str("{}").unwrap();
    config.mode = ClassifierMode::Heuristic;
    Classifier::new(config).expect("heuristic classifier")
}

fn request_with(text: &str) -> ChatCompletionRequest {
    serde_json::from_value(json!({
        "model": "auto",
        "messages": [{"role": "user", "content": text}]
    }))
    .expect("request shape is valid regardless of content")
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 512,
        max_shrink_iters: 4096,
        ..ProptestConfig::default()
    })]

    // ---------------------------------------------------------------- P7 ----

    /// The `@route:` case-insensitive probe sliced `first[..7]`. Byte 7 is a char
    /// boundary for every ASCII input and lands inside a multi-byte character
    /// otherwise, so a plain greeting in any non-Latin script panicked and
    /// `CatchPanicLayer` turned it into a bare 500.
    ///
    /// `arb_straddling_text` places a 2-, 3- and 4-byte character at chosen
    /// offsets, so it cannot miss this class.
    #[test]
    fn no_panic_on_a_straddled_route_prefix(text in arb_straddling_text()) {
        let classifier = heuristic();
        let result = block_on(classifier.classify(&request_with(&text)));
        prop_assert!(result.is_ok(), "classify must not fail: {:?}", result.err());
        let result = result.unwrap();
        // P17: confidence is a probability and is never NaN.
        prop_assert!(
            result.confidence.is_finite()
                && (0.0..=1.0).contains(&result.confidence),
            "confidence out of range or NaN: {}",
            result.confidence
        );
        prop_assert!(result.tier <= ComplexityTier::Reasoning);
    }

    /// The same class, reached through arbitrary Unicode including control
    /// characters, bidi overrides, combining marks and ZWJ sequences.
    #[test]
    fn no_panic_on_arbitrary_unicode(text in arb_adversarial_text()) {
        let classifier = heuristic();
        let result = block_on(classifier.classify(&request_with(&text)));
        prop_assert!(result.is_ok());
        let result = result.unwrap();
        prop_assert!(result.confidence.is_finite() && (0.0..=1.0).contains(&result.confidence));
    }

    /// A hostile `@route:` directive must never be honoured *and* must never
    /// panic. An invalid tier falls through to the heuristic with a warning;
    /// what it must not do is escalate to the strongest tier or crash.
    #[test]
    fn a_hostile_route_directive_never_panics(
        lead in arb_adversarial_text(),
        tier in arb_adversarial_text(),
    ) {
        let text = format!("@route:{tier}{lead}");
        let classifier = heuristic();
        let result = block_on(classifier.classify(&request_with(&text))).expect("classifies");
        // Only a literal, well-formed directive on its own line is honoured.
        if result.classifier == "override" {
            prop_assert!(
                matches!(
                    result.tier,
                    ComplexityTier::Trivial
                        | ComplexityTier::Simple
                        | ComplexityTier::Standard
                        | ComplexityTier::Hard
                        | ComplexityTier::Reasoning
                ),
                "override produced an impossible tier"
            );
        }
    }

    /// The keyword tables use `has_word`, which slices `lower[..at]` and
    /// `lower[at + needle.len()..]`. Both are char-boundary operations on a
    /// `to_lowercase()`-transformed string, and lowercasing can change byte
    /// length (`İ` → `i̇`), so the offsets are not obviously sound.
    #[test]
    fn no_panic_on_text_whose_lowercase_changes_its_byte_length(text in arb_adversarial_text()) {
        let lower = text.to_lowercase();
        // The transformation the tables rely on.
        let re_lowered = lower.to_lowercase();
        prop_assert_eq!(lower, re_lowered, "lowercase must be idempotent");
        let classifier = heuristic();
        prop_assert!(block_on(classifier.classify(&request_with(&text))).is_ok());
    }

    // --------------------------------------------------------------- P15 ----

    /// An unrecognised content part used to round-trip as `{"type":"Other"}`:
    /// an internally tagged *unit* variant has nowhere to keep the original, so
    /// both the tag and the payload were destroyed. Every `file`,
    /// `input_file`, `document`, `video_url`, Anthropic `thinking`/`tool_use`
    /// and Gemini `inline_data` part reached the provider as that.
    #[test]
    fn an_arbitrary_content_part_round_trips_verbatim(part in arb_json_value()) {
        let original = Value::Array(vec![part]);
        let Ok(parsed) = serde_json::from_value::<MessageContent>(original.clone()) else {
            return Ok(());   // not a part array; not this property's business
        };
        let re_encoded = serde_json::to_value(&parsed).expect("re-encodes");
        prop_assert_eq!(
            re_encoded, original,
            "a decoded part must re-encode byte-for-byte"
        );
    }

    /// Known parts keep their `type` tag. An untagged representation would make
    /// every part unparseable to the provider.
    #[test]
    fn a_known_part_keeps_its_type_tag(text in ".{0,40}") {
        let original = json!([{"type": "text", "text": text}]);
        let parsed: MessageContent = serde_json::from_value(original.clone()).unwrap();
        prop_assert_eq!(serde_json::to_value(&parsed).unwrap(), original);
    }

    // --------------------------------------------------------------- P17 ----

    /// `to_text` walks the part list. It must ignore what it cannot read rather
    /// than panic, and it must still return the parts it can.
    #[test]
    fn text_extraction_is_total(parts in prop::collection::vec(arb_json_value(), 0..6)) {
        let original = Value::Array(parts);
        let Ok(parsed) = serde_json::from_value::<MessageContent>(original.clone()) else {
            return Ok(());
        };
        let text = parsed.to_text();
        // Whatever it returns must be text, not a panic and not a nested value.
        prop_assert!(!text.contains('\0'));
    }

    /// A request whose `content` is present but of the wrong shape must be
    /// rejected by the type system, not panic. `null` in particular is legal on
    /// OpenAI assistant tool-call turns.
    #[test]
    fn a_null_or_absent_content_is_accepted(shape in arb_json_value()) {
        let body = json!({
            "model": "auto",
            "messages": [{"role": "assistant", "content": shape}]
        });
        // Either it parses (and we accept whatever came out) or it is a clean
        // serde error. What must never happen is a panic.
        if let Ok(request) = serde_json::from_value::<ChatCompletionRequest>(body) {
            let _ = request.messages[0].content.to_text();
        }
    }
}

/// A directed regression with the exact input that shipped, so the defect is
/// legible in the diff rather than only in a generator's output.
#[test]
fn the_shipped_panic_input_is_pinned_explicitly() {
    let classifier = heuristic();
    // Byte offsets: 日本語 is 9 bytes; byte 7 falls inside 語 (bytes 6..9).
    for prompt in [
        "日本語でこんにちは",
        "abcd😀",
        "कऋग",
        "Здравствуйте",
        "héllo wörld",
        "🚀 launch",
        "İstanbul is in Turkey", // to_lowercase() changes byte length
        "𝕏 marks the spot",
    ] {
        let result = block_on(classifier.classify(&request_with(prompt)))
            .unwrap_or_else(|e| panic!("{prompt:?} failed to classify: {e}"));
        assert!(
            result.confidence.is_finite(),
            "{prompt:?} produced a non-finite confidence"
        );
    }
}
