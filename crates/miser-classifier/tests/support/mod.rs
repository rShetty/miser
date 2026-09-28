//! Adversarial input strategies.
//!
//! The generator that shipped with `invariants.rs` drew from a 77-word ASCII
//! list, and its stated ambition was to "catch the next phrasing nobody thought
//! to write down". It could not: every literal in it was ASCII, which made the
//! char-boundary panic in the `@route:` probe — the defect that returned HTTP 500
//! to any request containing a Japanese greeting — structurally unreachable. A
//! generator that cannot express the input class cannot find the bug in it.
//!
//! These strategies are built backwards from the *classes* of defect that have
//! actually occurred, plus the classes that are structurally easy to get wrong in
//! Rust:
//!
//! * **char boundaries** — fixed-width byte slicing, `&s[..n]`, and any
//!   `char_indices` assumption. Target: a multi-byte character straddling a
//!   chosen byte offset.
//! * **empty and single-element** — `Vec::remove(0)`, `[0]`, `last()`.
//! * **type confusion** — a JSON field of the wrong type where a string is
//!   expected. `Value::as_*` returns `None`, which for a quota field reads as
//!   "no restriction".
//! * **numeric edges** — `NaN`, `±inf`, `-0.0`, subnormals, and values exactly on
//!   a comparison boundary. Every `<=` is false against `NaN`.
//! * **untrusted text** — control characters, lone newlines, combining marks,
//!   bidi overrides and ZWJ sequences.
//!
//! Every strategy here is total: it cannot panic or fail to produce a value.

// Each integration-test binary compiles this module separately, so a
// strategy used by only one of them reads as dead code in the others.
#![allow(dead_code)]
use proptest::prelude::*;
use serde_json::Value;

/// Multi-byte characters, shortest first: 2 bytes covers Latin-1 supplement,
/// 3 covers CJK and Devanagari, 4 covers emoji and mathematical alphanumerics.
const WIDE_CHARS: [char; 8] = ['é', '語', 'क', '🚀', '𝕏', '中', 'Ω', '한'];

/// Control and format characters that have no business in a prompt but arrive in
/// real traffic: NUL, the bidi overrides that make text render differently from
/// how it reads, and the zero-width space.
const HOSTILE_CHARS: [char; 8] = [
    '\u{0}', '\u{7}', '\u{200b}', '\u{200e}', '\u{202e}', '\u{2066}', '\u{feff}', '\u{1b}',
];

/// ASCII fragments that put a byte offset at risk: the route directive, the tier
/// words the pattern tables key on, and the separators the definitional
/// patterns split on.
const TRIGGERS: [&str; 14] = [
    "@route:",
    "@ROUTE:",
    "yes or no",
    "true or false",
    "just answer ",
    "hello",
    "thanks",
    "prove",
    "derive",
    "algorithm",
    "improve",
    "approved",
    "one sentence",
    "capital",
];

/// Text drawn from the ASCII vocabulary the pattern tables key on, so the
/// generator reaches the matching paths rather than missing every table.
///
/// Retained from the original generator, which was right about *this*: the bug
/// was never that it missed the tables.
pub fn arb_table_text() -> impl Strategy<Value = String> {
    let words = prop::sample::select(
        [
            "add",
            "optimize",
            "configure",
            "split",
            "upgrade",
            "implement",
            "design",
            "write",
            "explain",
            "analyze",
            "prove",
            "deploy",
            "migrate",
            "refactor",
            "debug",
            "the",
            "a",
            "auth",
            "billing",
            "database",
            "service",
            "microservices",
            "endpoint",
            "endpoints",
            "query",
            "queries",
            "schema",
            "capital",
            "unicode",
            "restaurant",
            "interest",
            "barcode",
            "api",
            "code",
            "rest",
            "git",
            "status",
            "diff",
            "http",
            "postmortem",
            "outage",
            "complexity",
            "recurrence",
            "amortized",
            "terraform",
            "nginx",
            "orm",
            "tracing",
            "thanks",
            "ok",
            "perfect",
            "got",
            "hi",
            "hello",
            "yes",
            "no",
            "yep",
            "route",
            "hard",
            "trivial",
            "standard",
            "simple",
            "reasoning",
            "monolith",
            "n+1",
            "logging",
            "ids",
            "semicolons",
            "braces",
            "awk",
            "sql",
            "regex",
            "leap",
            "year",
            "created",
            "many",
            "sentence",
            "word",
        ]
        .map(str::to_owned)
        .to_vec(),
    );
    prop::collection::vec(words, 0..14).prop_map(|ws| ws.join(" "))
}

/// Arbitrary Unicode text: printable, hostile, or a keyword fragment.
///
/// This is the generator that would have found the char-boundary panic on its
/// own, and it is also the one that can find the *next* fixed-width slice someone
/// writes.
pub fn arb_adversarial_text() -> impl Strategy<Value = String> {
    fn lit(s: &'static str) -> BoxedStrategy<String> {
        Just(s.to_owned()).boxed()
    }
    let leaf = prop_oneof![
        // Any single char: the whole Unicode range, including planes that only
        // multi-byte encodings reach.
        any::<char>().prop_map(String::from).boxed(),
        prop::sample::select(TRIGGERS.to_vec())
            .prop_map(String::from)
            .boxed(),
        prop::sample::select(HOSTILE_CHARS.to_vec())
            .prop_map(String::from)
            .boxed(),
        lit(" "),
        lit("\n"),
        lit("\r\n"),
        lit("\t"),
        lit("?"),
        lit("!"),
        lit("."),
        lit(":"),
        lit(","),
        lit("-"),
        lit("\u{2013}"),
        lit("*"),
        lit("`"),
    ]
    .boxed();
    prop::collection::vec(leaf, 0..24).prop_map(|parts| parts.concat())
}

/// Text with a multi-byte character placed at a chosen byte offset.
///
/// This is the exact shape of the shipped defect: `first[..7]` is safe for every
/// ASCII input and panics for precisely this input. A generator that emits
/// "arbitrary text" would hit it by luck; this one cannot miss.
pub fn arb_straddling_text() -> impl Strategy<Value = String> {
    (
        prop::collection::vec(0..=10usize, 1..=5),
        prop::collection::vec(0..WIDE_CHARS.len(), 1..=5),
        prop::sample::select(TRIGGERS.to_vec()),
    )
        .prop_map(|(leads, picks, trigger)| {
            let mut s = String::new();
            for (n, pick) in leads.iter().zip(picks.iter()) {
                s.push_str(&"a".repeat(*n));
                s.push_str(trigger);
                s.push(WIDE_CHARS[*pick % WIDE_CHARS.len()]);
            }
            s
        })
}

/// A JSON value of the wrong type for a field — the `Value::as_*` defect class.
pub fn arb_json_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        any::<bool>().prop_map(Value::from).boxed(),
        any::<i64>().prop_map(Value::from).boxed(),
        any::<f64>().prop_map(Value::from).boxed(),
        (0..32u32).prop_map(Value::from).boxed(),
        "[a-z0-9_/.:-]{0,24}".prop_map(Value::from).boxed(),
        Just(Value::Null).boxed(),
    ]
    .boxed();
    prop::collection::vec(leaf, 0..4).prop_map(Value::Array)
}
