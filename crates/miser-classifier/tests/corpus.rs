//! Corpus gates.
//!
//! The eval corpora existed but nothing asserted on them: `cargo test` never
//! read them and the evals tool only *printed* accuracy, so a tier change could
//! regress 12% of routing decisions with CI green. This makes them gates.
//!
//! Three corpora, deliberately treated differently:
//!
//! * `cases.jsonl` is hand-curated and small. It is held at 100% exact --
//!   every disagreement is a real bug in either the code or the expectation,
//!   and there is no noise to discount.
//! * `classifier_cases.jsonl` and `classifier_cases_large.jsonl` are generated.
//!   The large one is 83.5% duplicate rows (347 unique prompts behind 2100
//!   rows) and its labels are not hand-checked, so demanding 100% of it would
//!   mean encoding label noise as truth. They are held at a committed floor
//!   instead, so a regression still fails the build but an improvement has to
//!   be promoted deliberately rather than drifting upward unnoticed.
//!
//! Determinism is asserted separately, because it is cheap and because a
//! classifier that flips tiers between identical requests is a much harder
//! class of bug to diagnose later.

use miser_classifier::Classifier;
use miser_types::{ChatCompletionRequest, ClassifierConfig, ClassifierMode, ComplexityTier};
use serde::Deserialize;
use std::{collections::BTreeMap, fmt::Write as _, path::PathBuf};

#[derive(Debug, Deserialize)]
struct Case {
    id: String,
    expected_tier: ComplexityTier,
    request: ChatCompletionRequest,
}

/// Minimum exact-match accuracy for the generated corpora. Raise these
/// deliberately as the classifier improves; never lower them to make a build
/// pass without first understanding what regressed.
const FLOOR_CLASSIFIER_CASES: f64 = 0.76;
const FLOOR_LARGE_CASES: f64 = 0.78;

fn corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../evals")
        .join(name)
}

fn load(name: &str) -> Vec<Case> {
    let path = corpus(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str::<Case>(l)
                .unwrap_or_else(|e| panic!("bad case in {}: {e}\n  {l}", path.display()))
        })
        .collect()
}

fn heuristic() -> Classifier {
    let mut config: ClassifierConfig = serde_json::from_str("{}").unwrap();
    config.mode = ClassifierMode::Heuristic;
    config.confidence_threshold = 0.65;
    Classifier::new(config).expect("heuristic classifier")
}

/// Tier order, for classifying a miss as under- or over-routing.
fn rank(tier: ComplexityTier) -> usize {
    match tier {
        ComplexityTier::Trivial => 0,
        ComplexityTier::Simple => 1,
        ComplexityTier::Standard => 2,
        ComplexityTier::Hard => 3,
        ComplexityTier::Reasoning => 4,
    }
}

fn prompt_of(case: &Case) -> String {
    case.request
        .messages
        .iter()
        .map(|m| m.content.to_text())
        .collect::<Vec<_>>()
        .join(" | ")
}

/// The curated corpus is the contract: every case must be exact.
#[tokio::test]
async fn curated_corpus_is_exact() {
    let cases = load("cases.jsonl");
    assert!(cases.len() >= 60, "corpus shrank to {} cases", cases.len());

    let classifier = heuristic();
    let mut misses = String::new();
    for case in &cases {
        let got = classifier.classify(&case.request).await.unwrap();
        if got.tier != case.expected_tier {
            let _ = writeln!(
                misses,
                "  {} expected {:?} got {:?} ({:?})\n      {:?}",
                case.id,
                case.expected_tier,
                got.tier,
                got.reasons,
                prompt_of(case)
            );
        }
    }
    assert!(
        misses.is_empty(),
        "{} of {} curated cases regressed:\n{}",
        misses
            .lines()
            .filter(|l| l.starts_with("  ") && !l.starts_with("      "))
            .count(),
        cases.len(),
        misses
    );
}

/// Generated corpora: held to a floor, and any drop is a real regression.
#[tokio::test]
async fn generated_corpora_clear_their_floor() {
    for (name, floor) in [
        ("classifier_cases.jsonl", FLOOR_CLASSIFIER_CASES),
        ("classifier_cases_large.jsonl", FLOOR_LARGE_CASES),
    ] {
        let cases = load(name);
        let classifier = heuristic();
        let mut exact = 0usize;
        let mut under = 0usize;
        let mut over = 0usize;
        let mut misses = BTreeMap::new();
        for case in &cases {
            let got = classifier.classify(&case.request).await.unwrap();
            if got.tier == case.expected_tier {
                exact += 1;
            } else {
                match rank(got.tier).cmp(&rank(case.expected_tier)) {
                    std::cmp::Ordering::Less => under += 1,
                    std::cmp::Ordering::Greater => over += 1,
                    std::cmp::Ordering::Equal => unreachable!(),
                }
                *misses
                    .entry((case.expected_tier, got.tier))
                    .or_insert(0usize) += 1;
            }
        }
        let accuracy = exact as f64 / cases.len() as f64;
        // Sorted by count so the failure message is stable run to run.
        let mut worst: Vec<_> = misses.into_iter().collect();
        worst.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let mut detail = String::new();
        for ((expected, got), n) in worst.iter().take(5) {
            let _ = writeln!(detail, "  {n:>5}x expected {expected:?} got {got:?}");
        }
        assert!(
            accuracy >= floor,
            "{name}: exact accuracy {accuracy:.4} is below the committed floor {floor:.4} \
             (under-routing {under}, over-routing {over}, missed {} of {})\n{worst_classes}{detail}",
            cases.len() - exact,
            cases.len(),
            worst_classes = if detail.is_empty() {
                ""
            } else {
                "worst classes:\n"
            },
        );
    }
}

/// Identical requests must classify identically, every time.
#[tokio::test]
async fn classification_is_deterministic() {
    let classifier = heuristic();
    for name in ["cases.jsonl", "classifier_cases.jsonl"] {
        for case in load(name) {
            let first = classifier.classify(&case.request).await.unwrap();
            for _ in 0..4 {
                let again = classifier.classify(&case.request).await.unwrap();
                assert_eq!(
                    (first.tier, first.reasons.clone(), first.confidence),
                    (again.tier, again.reasons.clone(), again.confidence),
                    "{name}/{} is not deterministic: {:?} then {:?}",
                    case.id,
                    first.tier,
                    again.tier
                );
            }
        }
    }
}

/// Ceiling on the share of corpus cases that receive a tier with **no reason**.
///
/// An unexplained tier is the classifier's base score being returned because no
/// pattern matched, and 300 of the 330 cases currently in that state are tiered
/// wrongly -- hard work (postmortems, 200-microservice observability, ORM
/// upgrades) served by the cheap model. That is the expensive direction, so it
/// is worth measuring even while it is being fixed.
///
/// This is a ratchet, not a target: it may only go down. Tightening the pattern
/// tables should lower it, and every such commit should lower this number too.
/// Raising it needs a comment saying what regressed and why.
const MAX_UNEXPLAINED_RATE: f64 = 0.16;

/// Tier is total: every corpus case yields a tier and never errors, and the
/// share of tiers nothing explains stays under the ratchet.
#[tokio::test]
async fn every_case_yields_a_tier() {
    let classifier = heuristic();
    let mut unexplained = String::new();
    let mut unexplained_count = 0usize;
    let mut errored = String::new();
    let mut total = 0usize;
    for name in ["cases.jsonl", "classifier_cases_large.jsonl"] {
        for case in load(name) {
            total += 1;
            match classifier.classify(&case.request).await {
                Err(e) => {
                    let _ = writeln!(errored, "  {name}/{} errored: {e}", case.id);
                }
                Ok(got) => {
                    if got.reasons.is_empty() {
                        unexplained_count += 1;
                        let _ = writeln!(
                            unexplained,
                            "  {name}/{} -> {:?} with no reason: {:?}",
                            case.id,
                            got.tier,
                            prompt_of(&case)
                        );
                    }
                }
            }
        }
    }
    assert!(
        errored.is_empty(),
        "some cases failed to classify at all:\n{errored}"
    );

    let rate = unexplained_count as f64 / total as f64;
    assert!(
        rate <= MAX_UNEXPLAINED_RATE,
        "{unexplained_count} of {total} cases ({rate:.4}) were given a tier that nothing \
         explains, above the committed ratchet of {MAX_UNEXPLAINED_RATE:.4}.\n\
         The base score is being returned because no pattern matched, and most of these \
         are tiered wrongly. The ratchet may only go down; if this number rose, a pattern \
         table lost coverage.\n{unexplained}"
    );
}
