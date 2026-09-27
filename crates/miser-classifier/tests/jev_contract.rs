//! Formal tests for the Jev classification contract.
//!
//! The Jev path is where the expensive decisions are made and where every bug
//! found so far has lived: a missing `confidence` laundered into 0.70, a
//! code-fenced answer discarded, a `null` accepted as a probability. All three
//! were found by reading, not by testing, which is the gap this file closes.
//!
//! Two layers:
//!
//! * **Exhaustive** over a hand-built matrix of malformed responses. The shapes
//!   are the ones a real endpoint emits when it is unhealthy, and each one names
//!   the specific failure it is there to prevent.
//! * **Property-based** over generated responses, for the invariants that must
//!   hold for *every* input rather than for the ones I thought to enumerate.
//!
//! The properties are the contract:
//!
//! 1. No input panics. The classifier is on the request path; a panic here is a
//!    dropped request.
//! 2. Confidence is always a probability in [0, 1], and is never *invented*.
//!    A response carrying no confidence signal must fall back, not be assigned
//!    the default -- that inversion is what let a Jev outage look healthy.
//! 3. A Jev answer is only ever reported as `jev` if it was both a valid tier
//!    and a valid probability. Availability wins over accuracy, so falling back
//!    is always allowed; silently accepting a malformed answer is not.
//!
//! Everything runs against a mock endpoint, so these are hermetic and cost
//! nothing. `live_jev_contract` is the opt-in check against the real service.

use miser_classifier::Classifier;
use miser_types::{ChatCompletionRequest, ClassifierConfig, ClassifierMode, ComplexityTier};
use proptest::prelude::*;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

/// Serve one canned body, then stop. Mirrors the in-crate mock but is
/// self-contained so the properties can drive it directly.
async fn serve_once(body: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let Ok((mut sock, _)) = listener.accept().await else {
            return;
        };
        let mut buf = [0u8; 8192];
        let mut data = Vec::new();
        loop {
            let n = sock.read(&mut buf).await.unwrap_or(0);
            data.extend_from_slice(&buf[..n]);
            if data.windows(4).any(|w| w == b"\r\n\r\n") || n == 0 {
                break;
            }
        }
        let payload = body.into_bytes();
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            payload.len()
        );
        let _ = sock.write_all(head.as_bytes()).await;
        let _ = sock.write_all(&payload).await;
        let _ = sock.flush().await;
    });
    format!("http://{addr}")
}

fn jev_config(base_url: &str) -> ClassifierConfig {
    let mut config: ClassifierConfig = serde_json::from_str("{}").unwrap();
    config.mode = ClassifierMode::Jev;
    config.confidence_threshold = 0.65;
    config.jev.enabled = true;
    config.jev.model = "jev-latest".into();
    config.jev.base_url = base_url.into();
    config.jev.api_key = Some("test-key".into());
    config.jev.timeout_ms = 5_000;
    config
}

fn request() -> ChatCompletionRequest {
    serde_json::from_value(json!({
        "model": "auto",
        "messages": [{"role": "user", "content": "design a resilient multi-region migration"}]
    }))
    .unwrap()
}

/// A well-formed Jev envelope, so each malformed case differs in exactly one way.
fn envelope(tier: Value) -> String {
    json!({
        "model": "jev-1.13.0",
        "answers": {
            "tier": tier,
            "task": {"type": "choice", "choice": "coding"}
        },
        "usage": {"input_tokens": 40, "output_tokens": 8}
    })
    .to_string()
}

/// Property 3, exhaustively: each of these must NOT be accepted as a confident
/// `jev` answer. The comment on each row is the failure it prevents.
#[tokio::test]
async fn malformed_jev_responses_never_become_confident_jev_answers() {
    let cases: Vec<(&str, String)> = vec![
        // No confidence and no probabilities: must not inherit default_confidence().
        (
            "bare choice",
            envelope(json!({"type": "choice", "choice": "hard"})),
        ),
        (
            "null confidence",
            envelope(json!({"type": "choice", "choice": "hard", "confidence": null})),
        ),
        // A stringified number is not a number.
        (
            "stringified confidence",
            envelope(json!({"type": "choice", "choice": "hard", "confidence": "0.95"})),
        ),
        (
            "bool confidence",
            envelope(json!({"type": "choice", "choice": "hard", "confidence": true})),
        ),
        // probabilities present but missing the chosen key.
        (
            "probabilities miss chosen key",
            envelope(
                json!({"type": "choice", "choice": "hard", "probabilities": {"standard": 0.9}}),
            ),
        ),
        (
            "empty probabilities",
            envelope(json!({"type": "choice", "choice": "hard", "probabilities": {}})),
        ),
        // Tier is not one of ours.
        (
            "unknown tier",
            envelope(json!({"type": "choice", "choice": "mega-tier", "confidence": 0.9})),
        ),
        (
            "tier not a string",
            envelope(json!({"type": "choice", "choice": 7, "confidence": 0.9})),
        ),
        (
            "tier missing",
            envelope(json!({"type": "choice", "confidence": 0.9})),
        ),
        // Envelope broken at a higher level.
        (
            "answers missing",
            json!({"model": "jev-1.13.0", "usage": {"input_tokens": 1}}).to_string(),
        ),
        (
            "answers not an object",
            json!({"answers": "hard"}).to_string(),
        ),
        (
            "tier not an object",
            json!({"answers": {"tier": "hard"}}).to_string(),
        ),
        // Not JSON at all, in the shapes an unhealthy endpoint really emits.
        ("empty body", String::new()),
        (
            "html error page",
            "<html><body>502 Bad Gateway</body></html>".into(),
        ),
        ("null", "null".into()),
        ("json array", "[1,2,3]".into()),
        (
            "truncated json",
            r#"{"answers":{"tier":{"choice":"hard""#.into(),
        ),
        // The fence case, which is a *valid* answer that must be recovered.
        (
            "fenced json",
            "```json\n{\"tier\":\"hard\",\"confidence\":0.9}\n```".into(),
        ),
    ];

    for (name, body) in cases {
        let base = serve_once(body.clone()).await;
        let classifier = Classifier::new(jev_config(&base)).unwrap();
        let result = classifier.classify(&request()).await.unwrap();

        assert!(
            result.classifier != "jev",
            "{name}: malformed response was accepted as a confident jev answer \
             (tier={:?} confidence={})",
            result.tier,
            result.confidence
        );
        // Whatever it decided, it must be a real tier and a real probability.
        assert!(
            (0.0..=1.0).contains(&result.confidence),
            "{name}: confidence {} is not a probability",
            result.confidence
        );
    }
}

/// Property 3, positively: the well-formed shapes must still be accepted, or
/// the malformed matrix above would pass by rejecting everything.
#[tokio::test]
async fn well_formed_jev_responses_are_accepted() {
    let cases = vec![
        (
            "explicit confidence",
            envelope(json!({"type": "choice", "choice": "hard", "confidence": 0.9})),
        ),
        (
            "probabilities only",
            envelope(json!({"type": "choice", "choice": "hard", "probabilities": {"hard": 0.9}})),
        ),
        (
            "fenced with language tag",
            format!(
                "```json\n{}\n```",
                envelope(json!({"type": "choice", "choice": "hard", "confidence": 0.9}))
            ),
        ),
        (
            "fenced without language tag",
            format!(
                "```\n{}\n```",
                envelope(json!({"type": "choice", "choice": "hard", "confidence": 0.9}))
            ),
        ),
        (
            "fenced with surrounding prose",
            format!(
                "Here you go:\n```json\n{}\n```\nHope that helps.",
                envelope(json!({"type": "choice", "choice": "hard", "confidence": 0.9}))
            ),
        ),
    ];

    for (name, body) in cases {
        let base = serve_once(body).await;
        let classifier = Classifier::new(jev_config(&base)).unwrap();
        let result = classifier.classify(&request()).await.unwrap();
        assert_eq!(
            result.classifier, "jev",
            "{name}: a valid Jev answer was rejected and fell back"
        );
        assert_eq!(result.tier, ComplexityTier::Hard, "{name}: wrong tier");
    }
}

/// Property 1 and 2, by generation: no input may panic, and confidence must
/// always be a probability. The generator is deliberately loose -- it can emit
/// structurally valid JSON of the wrong shape, which is what a real endpoint
/// does when it is degrading.
/// `proptest!` generates a synchronous function, so the async body is driven
/// through a shared runtime. A fresh runtime per case would dominate the run
/// time; one long-lived runtime keeps proptest's shrinking usable.
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RT.get_or_init(|| tokio::runtime::Runtime::new().expect("runtime"))
        .block_on(fut)
}

// A recursive value generator: any JSON the endpoint could plausibly return.
fn arb_json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| json!(n)),
        any::<f64>().prop_map(|n| json!(n)),
        "[a-z]{0,12}".prop_map(Value::String),
    ];
    leaf.prop_recursive(3, 24, 3, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            prop::collection::hash_map("[a-z]{0,8}", inner, 0..4)
                .prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    #[test]
    fn no_panic_and_confidence_is_a_probability(payload in arb_json()) {
        let (confidence, classifier, supplied) = block_on(async {
            let base = serve_once(payload.to_string()).await;
            let classifier = Classifier::new(jev_config(&base)).unwrap();

            // The load-bearing assertion: this must not panic.
            let Ok(result) = classifier.classify(&request()).await else {
                return (-1.0, String::new(), false);
            };

            let supplied = payload
                .pointer("/answers/tier/confidence")
                .and_then(Value::as_f64)
                .or_else(|| {
                    payload
                        .pointer("/answers/tier/probabilities")
                        .and_then(Value::as_object)
                        .and_then(|p| p.get(tier_key(result.tier)))
                        .and_then(Value::as_f64)
                })
                .is_some();

            (result.confidence, result.classifier, supplied)
        });

        prop_assert!(
            (0.0..=1.0).contains(&confidence),
            "confidence {confidence} is not a probability for payload {}",
            payload
        );

        // If it claims to be a Jev answer, it must carry a confidence the
        // response actually supplied -- never the 0.70 default.
        if classifier == "jev" {
            prop_assert!(
                supplied,
                "reported as a confident jev answer with no confidence in the payload: {}",
                payload
            );
        }
    }
}

/// The wire key for a tier, which is lower-case (`reasoning`, not `Reasoning`).
fn tier_key(tier: ComplexityTier) -> &'static str {
    match tier {
        ComplexityTier::Trivial => "trivial",
        ComplexityTier::Simple => "simple",
        ComplexityTier::Standard => "standard",
        ComplexityTier::Hard => "hard",
        ComplexityTier::Reasoning => "reasoning",
    }
}

/// The confidence clamp is a real invariant, not an implementation detail:
/// a probability of 1.0 is still not certainty worth routing on.
#[tokio::test]
async fn confidence_is_clamped_into_the_probability_range() {
    for (raw, want_accepted) in [(1.4f64, true), (0.0, true), (-0.5, true)] {
        let base = serve_once(envelope(json!({
            "type": "choice", "choice": "hard", "confidence": raw
        })))
        .await;
        let classifier = Classifier::new(jev_config(&base)).unwrap();
        let result = classifier.classify(&request()).await.unwrap();
        assert!(
            (0.0..=1.0).contains(&result.confidence),
            "confidence {raw} escaped the range as {}",
            result.confidence
        );
        assert_eq!(
            result.classifier == "jev",
            want_accepted,
            "confidence {raw}: unexpected acceptance"
        );
    }
}

/// Opt-in: the real endpoint. Run with
/// `MISER_LIVE_JEV=1 cargo test -p miser-classifier --test jev_contract -- --ignored`
/// and `JEV_API_KEY` set. Deliberately `--ignored` so CI never depends on a
/// paid network call, while the contract can still be checked against reality.
#[tokio::test]
#[ignore = "requires JEV_API_KEY and network"]
async fn live_jev_contract() {
    let Ok(key) = std::env::var("JEV_API_KEY") else {
        panic!("JEV_API_KEY not set");
    };

    // The three primitives the router depends on, in one request, as the
    // contract says they must work.
    let body = json!({
        "model": "jev-latest",
        "state": {
            "request": "Recommend a restaurant in Berlin",
            "tools": [],
            "tool_history": false
        },
        "questions": {
            "tier": {
                "type": "choice",
                "instructions": "Which capability tier?",
                "criteria": {
                    "trivial": "greetings and bare facts",
                    "simple": "explain a concept",
                    "standard": "multi-file work",
                    "hard": "system design",
                    "reasoning": "formal proof"
                }
            },
            "is_bug": {
                "type": "noul",
                "instructions": "Is the user reporting a software defect?",
                "criteria": {
                    "true": "describes broken behaviour",
                    "false": "asking a question"
                }
            },
            "urgency": {
                "type": "score",
                "instructions": "How urgent?",
                "criteria": ["can wait", "this week", "blocking revenue"]
            }
        }
    })
    .to_string();

    const URL: &str = "https://api.typesafe.ai/v1/systemone";

    // Call the real endpoint directly and assert the documented shape, rather
    // than routing it through the mock, because the point is the live contract.
    let client = reqwest::Client::new();
    let response = client
        .post(URL)
        .bearer_auth(&key)
        .json(&serde_json::from_str::<Value>(&body).unwrap())
        .send()
        .await
        .expect("jev endpoint reachable");
    assert!(
        response.status().is_success(),
        "jev returned {}",
        response.status()
    );
    let payload: Value = response.json().await.expect("jev returned JSON");

    let answers = &payload["answers"];
    assert!(answers["tier"]["choice"].is_string(), "choice missing");
    assert!(
        answers["tier"]["confidence"]
            .as_f64()
            .is_some_and(|c| (0.0..=1.0).contains(&c)),
        "confidence out of range: {}",
        answers["tier"]["confidence"]
    );
    assert!(
        answers["is_bug"]["noul"]
            .as_f64()
            .is_some_and(|n| (0.0..=1.0).contains(&n)),
        "noul out of range: {}",
        answers["is_bug"]["noul"]
    );
    assert!(
        answers["urgency"]["score"].is_number(),
        "score missing: {}",
        answers["urgency"]["score"]
    );
    assert!(
        answers["urgency"]["legend"].is_object(),
        "score legend missing"
    );
}
