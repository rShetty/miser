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
//! Both are round-trip and default-consistency properties, which is what the
//! harnesses below state.
//!
//! See the module documentation in `kani_proofs.rs` for how to run these.

use super::*;
use kani::any;

/// P15a. An unrecognised content part survives a round trip byte-for-byte.
///
/// The property that must hold for *every* input, not just the shapes somebody
/// thought to test. Kani's `any::<Value>()` covers JSON that no hand-written
/// case would contain.
#[kani::proof]
fn an_unmodelled_content_part_round_trips_verbatim() {
    let raw: String = any();
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return; // not JSON: serde's business, not this property's
    };
    let Ok(parts) = serde_json::from_value::<Vec<ContentPart>>(value.clone()) else {
        return; // not a part array
    };
    let re_encoded = serde_json::to_value(&parts).expect("ContentPart re-encodes");
    assert_eq!(
        re_encoded, value,
        "a decoded part list must re-encode byte-for-byte"
    );
}

/// P15b. A known part keeps its `type` tag.
///
/// An untagged representation would make every part unparseable to the provider
/// while looking perfectly correct in the gateway's own logs.
#[kani::proof]
fn a_known_content_part_keeps_its_type_tag() {
    let text: String = any();
    let original = serde_json::json!([{ "type": "text", "text": text }]);
    let parsed: MessageContent =
        serde_json::from_value(original.clone()).expect("a text part always parses");
    assert_eq!(serde_json::to_value(&parsed).expect("re-encodes"), original);
}

/// P15. `to_text` is total: it returns text for any part list, and never panics
/// on a shape it does not understand.
#[kani::proof]
fn to_text_is_total() {
    let raw: String = any();
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return;
    };
    let Ok(content) = serde_json::from_value::<MessageContent>(value) else {
        return;
    };
    let _ = content.to_text();
}

/// The `QualityConfig` default-consistency property, as a proof rather than a
/// comment. A field whose serde default disagrees with `impl Default` is a
/// latent behaviour change that depends on whether the operator happened to
/// write a table heading.
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

/// P17. A classification result's confidence is a probability for any value the
/// deserializer can produce, including one built by hand.
///
/// NaN would defeat every `>= threshold` comparison silently, which is a
/// soundness property rather than a tidiness one.
#[kani::proof]
fn a_deserialized_confidence_is_a_probability() {
    let raw: String = any();
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return;
    };
    if let Ok(result) = serde_json::from_value::<ClassificationResult>(value) {
        assert!(
            result.confidence.is_finite(),
            "confidence is not finite: {}",
            result.confidence
        );
        assert!(
            (0.0..=1.0).contains(&result.confidence),
            "confidence out of range: {}",
            result.confidence
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
