//! Adversarial strategies for the gateway's property tests.
//!
//! Kept separate from `miser-classifier/tests/support` rather than shared: that
//! one is a `tests/` module of a different crate, and a dev-dependency cycle
//! would be worse than duplicating two strategies. Only the strategies the
//! gateway actually needs live here.

#![allow(dead_code)]
use proptest::prelude::*;
use serde_json::Value;

/// Floats on, or adjacent to, a comparison boundary.
///
/// The shipped band edges and their immediate IEEE neighbours, because an
/// inclusive-vs-exclusive boundary bug lives in exactly the gap between two
/// doubles. `NaN` and the infinities are included for P4c.
pub fn arb_boundary_float() -> impl Strategy<Value = f64> {
    prop_oneof![
        Just(0.0),
        Just(-0.0),
        Just(f64::MIN),
        Just(f64::MAX),
        Just(f64::MIN_POSITIVE),
        Just(f64::EPSILON),
        Just(f64::from_bits(1)),
        Just(f64::NAN),
        Just(f64::INFINITY),
        Just(f64::NEG_INFINITY),
        Just(0.035),
        Just(f64::from_bits(0.035f64.to_bits() - 1)),
        Just(f64::from_bits(0.035f64.to_bits() + 1)),
        Just(0.09),
        Just(0.36),
        Just(1.4),
        any::<f64>(),
        (0.0f64..1000.0).prop_map(|v| v / 1_000_000.0),
    ]
}

/// A model price as a provider might send it.
///
/// The interesting cases are the present-but-unreadable ones — `null`, an
/// object, a bool, an array, a non-numeric string — which the original extractor
/// silently mapped to 0.0 and which therefore made a paid model the cheapest
/// candidate in its band under `allow_free = true`.
pub fn arb_price_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        arb_boundary_float().prop_map(Value::from).boxed(),
        "[0-9.eE+-]{0,12}".prop_map(Value::from).boxed(),
        Just(Value::Null).boxed(),
        Just(Value::Array(vec![Value::from(1)])).boxed(),
        Just(serde_json::json!({"usd": 0.15})).boxed(),
        Just(Value::Bool(true)).boxed(),
    ]
    .boxed()
}

/// The quota-bearing fields of an admin key payload, which is where the
/// `Value::as_*` defect lived: a wrong-typed value reads as "not set", and for a
/// quota that means *unlimited* rather than *rejected*.
pub fn arb_quota_field() -> impl Strategy<Value = String> {
    prop::sample::select(
        [
            "rate_limit_rpm",
            "monthly_budget_usd",
            "allowed_tiers",
            "expires_at",
            "owner",
            "client",
        ]
        .map(str::to_owned)
        .to_vec(),
    )
}

/// An admin key payload carrying one quota field of an arbitrary JSON type.
pub fn arb_quota_payload() -> impl Strategy<Value = Value> {
    (
        arb_quota_field(),
        prop_oneof![
            any::<i64>().prop_map(Value::from).boxed(),
            any::<f64>().prop_map(Value::from).boxed(),
            any::<bool>().prop_map(Value::from).boxed(),
            Just(Value::Null).boxed(),
            Just(Value::from(0u64)).boxed(),
            Just(Value::from(4_294_967_296u64)).boxed(),
            "[a-zA-Z0-9_ ]{0,16}".prop_map(Value::from).boxed(),
            prop::collection::vec(any::<u32>(), 0..4)
                .prop_map(Value::from)
                .boxed(),
        ]
        .boxed(),
        arb_quota_field(),
        "[a-zA-Z0-9_ ]{0,12}".prop_map(Value::from),
    )
        .prop_map(|(field, value, owner_field, owner)| {
            let mut body = serde_json::Map::new();
            body.insert("owner".to_owned(), Value::from("acme"));
            body.insert(field, value);
            // A second benign field, so the payload is not a single-key object.
            body.insert(owner_field, owner);
            Value::Object(body)
        })
}

/// A non-negative finite float, for the properties that only make sense on a
/// real price. `NaN` and the infinities are filtered out by construction rather
/// than by a post-hoc `prop_filter`, which would silently reject a large share
/// of cases and make the test vacuous.
pub fn arb_finite_price() -> impl Strategy<Value = f64> {
    prop_oneof![
        Just(0.0),
        Just(f64::MIN_POSITIVE),
        Just(0.035),
        Just(0.09),
        Just(0.36),
        Just(1.4),
        (0.0f64..1_000.0),
        (0.0f64..1.0).prop_map(|v| v / 1_000_000.0),
    ]
}
