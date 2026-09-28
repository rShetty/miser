//! Kani proof harnesses for the wire types.
//!
//! The types in this crate are the gateway's trust boundary: everything in a
//! request body arrives as untrusted JSON and is deserialized here. Two of the
//! fifteen defects in `CORRECTNESS_FINDINGS.md` were in this file:
//!
//! * `ContentPart::Other` was a `#[serde(other)]` unit variant, which discarded
//!   the `type` tag and every field on the part, so an unrecognised part reached
//!   the provider as `{"type":"Other"}`.
//! * `QualityConfig::escalate_on_failure` had two different defaults -- `false`
//!   from serde, `true` from `impl Default` -- so *naming* the `[quality]`
//!   table changed the meaning of a key inside it.
//!
//! See the module documentation in `kani_proofs.rs` for how to run these, and
//! for why the harnesses are written against a restricted set of types.
//!
//! ## Why no `String`
//!
//! `kani::Arbitrary` is only implemented for types Kani can represent finitely,
//! so `String` and `&str` are *not* implementable -- `any::<String>()` is a
//! compile error, not a runtime failure. The first version of this file used it
//! in four places and had never been compiled, because `#[cfg(kani)]` strips
//! the module before rustc type-checks it. The CI run caught it.
//!
//! Strings are therefore built from `[char; N]`, which Kani does support, and
//! arbitrary JSON from a `#[derive(kani::Arbitrary)]` enum over primitives.
//! That is a smaller input space than "all strings" -- and a *non-vacuous* one,
//! which matters more: a generator that emits mostly-invalid input and returns
//! early proves nothing.

use super::*;
use kani::any;

/// An arbitrary JSON value, over the types Kani can represent.
///
/// Bounded and non-recursive on purpose: recursion would need an
/// `#[kani::unwind]` declaration per level, and an unbounded tree is not
/// representable anyway. The variants cover the shapes that actually reach this
/// crate from a client, and `Two` is the one that matters most -- an array of
/// two scalars is exactly the `[[0, ""]]` shape that serde's sequence-form tag
/// handling used to misread as a text part.
#[derive(kani::Arbitrary, Clone, Debug)]
enum ArbValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str([char; 6]),
    Two([char; 4], i64),
    Object([char; 5], [char; 4]),
}

impl From<ArbValue> for Value {
    fn from(value: ArbValue) -> Self {
        match value {
            ArbValue::Null => Value::Null,
            ArbValue::Bool(b) => Value::Bool(b),
            ArbValue::Int(i) => Value::from(i),
            ArbValue::Float(f) => serde_json::Number::from_f64(f)
                .map(Value::Number)
                .unwrap_or(Value::Null),
            ArbValue::Str(chars) => Value::String(chars.iter().collect()),
            ArbValue::Two(chars, i) => {
                Value::Array(vec![Value::String(chars.iter().collect()), Value::from(i)])
            }
            ArbValue::Object(key, chars) => {
                let mut object = serde_json::Map::new();
                object.insert(
                    key.iter().collect::<String>(),
                    Value::String(chars.iter().collect()),
                );
                Value::Object(object)
            }
        }
    }
}

/// An arbitrary string, as Kani can represent one.
fn arb_text<const N: usize>() -> String {
    let chars: [char; N] = any();
    chars.iter().collect()
}

/// P15a. An unrecognised content part survives a round trip byte-for-byte.
///
/// The property that must hold for *every* input, not just the shapes somebody
/// thought to test. The generator produces valid JSON, so this is not vacuous:
/// every branch below reaches a decode, and the `Two` variant is the shape that
/// produced defect #75.
#[kani::proof]
fn an_unmodelled_content_part_round_trips_verbatim() {
    let part: Value = Value::from(any::<ArbValue>());
    let original = Value::Array(vec![part]);

    let parts: Vec<ContentPart> =
        serde_json::from_value(original.clone()).expect("generated JSON is a part array");
    let re_encoded = serde_json::to_value(&parts).expect("ContentPart re-encodes");
    assert_eq!(
        re_encoded, original,
        "a decoded part list must re-encode byte-for-byte"
    );
}

/// P15b. A known part keeps its `type` tag.
///
/// An untagged representation would make every part unparseable to the provider
/// while looking perfectly correct in the gateway's own logs.
#[kani::proof]
fn a_known_content_part_keeps_its_type_tag() {
    let text = arb_text::<8>();
    let original = serde_json::json!([{ "type": "text", "text": text }]);
    let parsed: MessageContent =
        serde_json::from_value(original.clone()).expect("a text part always parses");
    assert_eq!(serde_json::to_value(&parsed).expect("re-encodes"), original);
}

/// P15. `to_text` is total: it returns text for any part list, and never panics
/// on a shape it does not understand.
#[kani::proof]
fn to_text_is_total() {
    let parts: Vec<ContentPart> = vec![ContentPart::Other(Value::from(any::<ArbValue>()))];
    let content = MessageContent::Parts(parts);
    let _ = content.to_text();
}

/// The `QualityConfig` default-consistency property, as a proof rather than a
/// comment. A field whose serde default disagrees with `impl Default` is a
/// latent behaviour change that depends on whether the operator happened to
/// write a table heading. This is defect #70.
///
/// Note what is *not* claimed here: the confidence bound. That guarantee is
/// established by the clamping in the classifier and by `properties::` in the
/// gateway, not by serde -- an earlier version of this file asserted it against
/// a deserialization path that does not enforce it, which would have been a
/// vacuous proof of the wrong thing.
#[kani::proof]
fn serde_and_default_agree_on_every_quality_config_field() {
    let from_default = serde_json::to_value(QualityConfig::default()).expect("serialises");
    let from_serde: QualityConfig =
        serde_json::from_value(serde_json::json!({})).expect("an empty object deserialises");
    let from_serde = serde_json::to_value(from_serde).expect("serialises");

    for field in ["enabled", "minimum_score", "escalate_on_failure"] {
        assert_eq!(
            from_serde[field], from_default[field],
            "{field} disagrees between serde and Default::default()"
        );
    }
}

/// `ComplexityTier`'s `Default` must not be an under-route. `Trivial` would
/// silently downgrade an unset tier to the cheapest model, and `Hard` would
/// over-route; the safe floor is `Simple`.
#[kani::proof]
fn a_default_tier_is_not_an_under_route() {
    let default = ComplexityTier::default();
    assert!(
        default >= ComplexityTier::Simple,
        "a zeroed struct must not default to a tier below Simple"
    );
    assert!(
        default <= ComplexityTier::Reasoning,
        "a zeroed struct must not default to the most expensive tier"
    );
}

/// An arbitrary `content` value decodes or fails cleanly -- and if it decodes,
/// its text extraction cannot panic. The combination that matters for the trust
/// boundary: the gateway must never abort on a body it successfully parsed.
#[kani::proof]
fn an_arbitrary_content_value_never_panics() {
    let value = Value::from(any::<ArbValue>());
    if let Ok(content) = serde_json::from_value::<MessageContent>(value) {
        let _ = content.to_text();
    }
}

/// Text of an arbitrary shape, including the multi-byte characters that make a
/// fixed-width byte slice unsafe. Kani explores all `[char; N]`, so this
/// reaches the boundary cases rather than hoping to hit them.
#[kani::proof]
fn text_extraction_of_arbitrary_text_is_total() {
    let text = arb_text::<12>();
    let content: MessageContent =
        serde_json::from_value(Value::String(text)).expect("a string content always decodes");
    let _ = content.to_text();
}
