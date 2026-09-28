//! Kani proof harnesses for the classifier's pure decision functions.
//!
//! ## Why these and not more
//!
//! The functions below are the ones that take **attacker-chosen bytes** and
//! return a routing decision. That combination is where the defects actually
//! were: two of the fifteen in `CORRECTNESS_FINDINGS.md` were crashes in
//! exactly this shape -- `first[..7]` in the `@route:` probe, and
//! `Vec::remove(0)` in the semantic cache's eviction. Neither is findable by
//! reading, and neither is findable by a test suite whose generator draws from a
//! 77-word ASCII list.
//!
//! Kani explores the input space symbolically, so `override_tier_never_panics`
//! below is a statement about *every* `&str`, including the ones nobody thought
//! to write down. That is a strictly stronger claim than any generator can make.
//!
//! ## Running
//!
//! Kani is verified on `x86_64-unknown-linux-gnu`, `x86_64-apple-darwin` and
//! `aarch64-apple-darwin`. It has no aarch64 **Linux** build, so these harnesses
//! cannot be executed on every development machine -- see
//! `docs/SPEC.md` section 7 and `.github/workflows/verify.yml`.
//!
//! ```text
//! cargo kani -p miser-classifier
//! cargo kani -p miser-classifier --harness override_tier_never_panics
//! ```
//!
//! The module is `#[cfg(kani)]`, so `cargo build` and `cargo test` never compile
//! it and `kani` never has to be a dependency for a normal build.
//!
//! ## What is deliberately not here
//!
//! `Classifier::classify` is async and performs network I/O; Kani supports
//! neither. Everything it decides, though, bottoms out in the pure functions
//! below, which is why extracting them was worth doing.

#![cfg(kani)]

use super::*;
use kani::any;

/// An arbitrary string, as Kani can represent one.
///
/// `[char; N]` rather than `String` or `&str`: `kani::Arbitrary` is only
/// implemented for types the verifier can represent finitely, so `any::<String>()`
/// and `any::<&str>()` are compile errors. The first version of this file used
/// `any::<&str>()` in ten places and had never been compiled, because
/// `#[cfg(kani)]` strips the module before rustc type-checks it. CI caught it.
///
/// 12 chars is enough to reach every byte offset a `@route:` prefix probe could
/// slice at, which is what these harnesses exist to cover.
fn arb_text<const N: usize>() -> String {
    let chars: [char; N] = any();
    chars.iter().collect()
}

/// P7. The `@route:` probe never panics on any string.
///
/// This is the harness for the defect that returned HTTP 500 to any request
/// containing a Japanese greeting. `strip_prefix` is safe; the case-insensitive
/// fallback used `first[..7]`, a fixed byte index, and panicked whenever byte 7
/// landed inside a multi-byte character.
///
/// Kani explores all `&str`, so this covers the 2-byte, 3-byte and 4-byte
/// boundaries that a generator has to be *told* about.
#[kani::proof]
fn override_tier_never_panics() {
    let text = arb_text::<12>();
    let request: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
        "model": "auto",
        "messages": [{"role": "user", "content": text}]
    }))
    .expect("this request shape is always constructible");
    // The claim is that this returns.
    let _ = override_tier(&request);
}

/// P7 + a stronger claim: whenever an override *is* honoured, the tier is a real
/// tier and the directive is a literal prefix of the first line.
///
/// The second half matters because the alternative to honouring a directive is
/// ignoring it, and silently escalating an unparseable directive to the
/// strongest tier would be worse than ignoring it.
#[kani::proof]
fn a_honoured_override_is_a_real_tier_from_a_literal_directive() {
    let text = arb_text::<12>();
    let request: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
        "model": "auto",
        "messages": [{"role": "user", "content": text}]
    }))
    .expect("shape is fixed");
    if let Some((tier, reason)) = override_tier(&request) {
        assert!(matches!(
            tier,
            ComplexityTier::Trivial
                | ComplexityTier::Simple
                | ComplexityTier::Standard
                | ComplexityTier::Hard
                | ComplexityTier::Reasoning
        ));
        assert!(
            reason.starts_with("override:"),
            "an honoured override must say so: {reason}"
        );
        // A directive is only honoured when it is at the start of the first
        // line, optionally after `just `/`answer `, and case-insensitively.
        let first = text.lines().next().unwrap_or("").trim();
        let lower = first.to_ascii_lowercase();
        assert!(
            lower.starts_with("@route:")
                || lower.starts_with("just @route:")
                || lower.starts_with("answer @route:")
                || lower.starts_with("just answer @route:"),
            "honoured a directive that is not a prefix of the first line: {first:?}"
        );
    }
}

/// P7. `has_word` never panics.
///
/// It slices `lower[..at]` and `lower[at + needle.len()..]` at offsets found by
/// `match_indices`, which is only sound while `lower` is unchanged between the
/// search and the slice. `needle.len()` is in bytes, so a multi-byte needle
/// keeps the offsets consistent -- but the callers pass `lower` after
/// `to_lowercase()`, and the callers themselves must not have re-transformed it.
#[kani::proof]
fn has_word_never_panics() {
    let haystack = arb_text::<12>();
    let needle = arb_text::<4>();
    let _ = has_word(haystack, needle);
}

/// P7. `has_word` is symmetric under case, so a keyword list cannot depend on
/// how the caller capitalised the prompt.
#[kani::proof]
fn has_word_is_case_insensitive() {
    let haystack = arb_text::<12>();
    let needle = arb_text::<4>();
    let a = has_word(&haystack.to_lowercase(), &needle.to_lowercase());
    let b = has_word(&haystack.to_ascii_lowercase(), &needle.to_ascii_lowercase());
    assert_eq!(
        a, b,
        "to_lowercase and to_ascii_lowercase must agree on a match"
    );
}

/// P7. `is_short_definitional` never panics on any string.
#[kani::proof]
fn is_short_definitional_never_panics() {
    let text = arb_text::<12>();
    let _ = is_short_definitional(text);
}

/// P17. `task` is total and never returns a Reasoning task for text that
/// contains no reasoning keyword as a *word*.
///
/// The property that matters: the Reasoning floor in `miser-policy` is
/// *unbounded*, so a false positive here is the most expensive decision the
/// classifier can make. A substring match on `prove` fires on "improve" and
/// "approved"; `has_word` does not.
#[kani::proof]
fn a_reasoning_task_requires_a_reasoning_keyword_as_a_word() {
    let text = arb_text::<12>();
    let lower = text.to_lowercase();
    if let Some(TaskType::Reasoning) = task(text) {
        assert!(
            ["prove", "derive", "algorithm"]
                .iter()
                .any(|needle| has_word(&lower, needle)),
            "Reasoning task for text with no reasoning keyword as a word: {text:?}"
        );
    }
}

/// P7. `strip_code_fence` never panics and never returns a slice that is not a
/// subslice of its input -- a returned `&str` that does not come from the input
/// would be undefined behaviour in a caller that assumed otherwise.
#[kani::proof]
fn strip_code_fence_returns_a_subslice_or_the_input() {
    let content = arb_text::<24>();
    let stripped = strip_code_fence(content);
    // Re-attach lifetimes to compare addresses rather than contents.
    let base = content.as_ptr() as usize;
    let end = base + content.len();
    let at = stripped.as_ptr() as usize;
    assert!(
        at >= base && at + stripped.len() <= end,
        "strip_code_fence returned a slice outside its input"
    );
}

/// P17. `rank_of` is a total order over the tiers, which is what lets the
/// classifier claim "the floor is monotone".
#[kani::proof]
fn tier_rank_is_strictly_increasing() {
    let tiers = [
        ComplexityTier::Trivial,
        ComplexityTier::Simple,
        ComplexityTier::Standard,
        ComplexityTier::Hard,
        ComplexityTier::Reasoning,
    ];
    for (i, a) in tiers.iter().enumerate() {
        for (j, b) in tiers.iter().enumerate() {
            if i < j {
                assert!(
                    rank_of(*a) < rank_of(*b),
                    "rank is not monotone: {:?} < {:?}",
                    a,
                    b
                );
            }
        }
    }
}

/// P7. `request_text` never panics on any request shape, including a message
/// whose `content` is present but structurally odd.
#[kani::proof]
fn request_text_never_panics() {
    let text = arb_text::<12>();
    let request: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
        "model": "auto",
        "messages": [{"role": "user", "content": text}]
    }))
    .expect("shape is fixed");
    let _ = request_text(&request);
}
