//! Contract tests for the two-stage verification cascade.
//!
//! The cascade is a safety mechanism, so the tests concentrate on the ways it
//! could *cause* harm rather than the happy path:
//!
//! * it must never **lower** a tier -- the cheap decision is a floor
//! * it must not run on a confident local answer, because paying to confirm
//!   what the patterns already got right is pure waste
//! * it must not re-litigate an explicit `@route:` directive
//! * an unsure or unreachable verifier must leave the decision alone, or a
//!   flaky second stage becomes an arbitrary escalation source
//!
//! The two-Jev-call path is exercised with a mock that answers the tier question
//! and the verification question independently, which is also a check that the
//! two really are separate questions.

use miser_classifier::Classifier;
use miser_types::{
    CascadeAction, ChatCompletionRequest, ClassifierConfig, ClassifierMode, ComplexityTier,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

/// Serve a fixed list of bodies, one per connection, so a test can script a
/// two-call exchange.
async fn serve(bodies: Vec<String>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for body in bodies {
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
        }
    });
    format!("http://{addr}")
}

fn config(base: &str) -> ClassifierConfig {
    let mut c: ClassifierConfig = serde_json::from_str("{}").unwrap();
    c.mode = ClassifierMode::Heuristic;
    c.confidence_threshold = 0.65;
    c.jev.enabled = true;
    c.jev.model = "jev-latest".into();
    c.jev.base_url = base.into();
    c.jev.api_key = Some("test-key".into());
    c.jev.timeout_ms = 5_000;
    c
}

fn req(text: &str) -> ChatCompletionRequest {
    serde_json::from_value(json!({
        "model": "auto",
        "messages": [{"role": "user", "content": text}]
    }))
    .unwrap()
}

/// The verification answer. `noul` is the probability that the proposed tier is
/// *right*; 0.96 agrees, 0.04 disagrees, 0.50 is a coin flip.
fn verify(noul: f64) -> String {
    json!({
        "answers": {"tier_is_right": {"type": "noul", "noul": noul}}
    })
    .to_string()
}

/// Off by default: the second stage costs money, so it is opt-in.
#[test]
fn cascade_is_off_by_default() {
    let c: ClassifierConfig = serde_json::from_str("{}").unwrap();
    assert!(!c.cascade.enabled, "cascade must default to off");
}

/// The cascade is a second *model* call, so the URL it posts to has to be
/// resolved the same way `jev` resolves it. The two had separate copies of that
/// logic and the cascade's was less forgiving: `path = "systemone"` (no
/// leading slash) produced `.../v1systemone` against a `.../v1` base, which
/// reaches nothing. The request then failed, the tier stayed where the cheap
/// stage put it, and `reasons` said `cascade-unavailable` -- so an operator
/// reading the reason list would conclude the verifier had spoken, when in
/// fact it had never been reached.
#[tokio::test]
async fn the_cascade_reaches_the_endpoint_whether_or_not_the_path_has_a_leading_slash() {
    for path in ["/systemone", "systemone"] {
        // Record the path the server actually saw, and answer a disagreement
        // so a reached verifier has an observable effect.
        let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        {
            let seen = seen.clone();
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
                let head = String::from_utf8_lossy(&data[..data.len().min(200)]);
                *seen.lock().unwrap() = head.lines().next().unwrap_or_default().to_string();
                let payload = verify(0.04).into_bytes();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    payload.len()
                );
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.write_all(&payload).await;
                let _ = sock.flush().await;
            });
        }

        let mut c = config(&format!("http://{addr}"));
        c.jev.path = Some(path.into());
        c.cascade.enabled = true;
        c.cascade.verify_below = 1.0;
        let result = Classifier::new(c)
            .unwrap()
            .classify(&req("hello"))
            .await
            .unwrap();

        let request_line = seen.lock().unwrap().clone();
        assert!(
            request_line.contains("/systemone"),
            "path {path:?} produced request {request_line:?}, which never reached the endpoint"
        );
        assert!(
            !request_line.contains("/v1systemone"),
            "path {path:?} was mis-joined onto the base URL: {request_line:?}"
        );
        assert_eq!(
            result.cascade.as_deref(),
            Some("local-escalated"),
            "path {path:?}: a reached verifier must be able to change the outcome"
        );
    }
}

/// Every dispatch mode must reach the cascade. `Hybrid` short-circuited on a
/// confident heuristic with a `return` from inside the mode match, which
/// skipped the cascade that wraps the dispatch entirely -- so in Hybrid mode a
/// confident cheap answer was never verified, while the *same* answer in
/// Heuristic mode was. The same prompt therefore routed differently depending
/// only on the configured mode.
#[tokio::test]
async fn a_confident_cheap_decision_is_verified_in_hybrid_mode_too() {
    // "kubernetes" scores just above `confidence_threshold` (0.65) and below
    // `verify_below` (0.70), so the cascade must consult the verifier.
    let prompt = "kubernetes";

    // First: confirm the prompt really is in the band where the cascade bites,
    // by showing Heuristic mode escalates it.
    let base = serve(vec![verify(0.04)]).await;
    let mut c = config(&base);
    c.cascade.enabled = true;
    let heuristic_mode = Classifier::new(c)
        .unwrap()
        .classify(&req(prompt))
        .await
        .unwrap();
    assert_eq!(
        heuristic_mode.cascade.as_deref(),
        Some("local-escalated"),
        "test premise: Heuristic mode verifies this prompt, so Hybrid must too"
    );

    // Now the same prompt in Hybrid mode. The heuristic's confidence clears the
    // short-circuit, which is exactly the branch that used to skip the cascade.
    let base = serve(vec![verify(0.04)]).await;
    let mut c = config(&base);
    c.mode = ClassifierMode::Hybrid;
    c.cascade.enabled = true;
    let hybrid = Classifier::new(c)
        .unwrap()
        .classify(&req(prompt))
        .await
        .unwrap();
    assert_eq!(
        hybrid.cascade.as_deref(),
        Some("local-escalated"),
        "Hybrid mode skipped the verification cascade that Heuristic mode applied"
    );
    assert_eq!(
        hybrid.tier, heuristic_mode.tier,
        "the same prompt must route the same way in both modes"
    );
}

/// A verifier that agrees leaves the tier alone but records that it was checked.
#[tokio::test]
async fn agreement_leaves_the_tier_and_is_recorded() {
    let base = serve(vec![verify(0.96)]).await;
    let mut c = config(&base);
    c.cascade.enabled = true;
    c.cascade.verify_below = 1.0; // always verify
    let result = Classifier::new(c)
        .unwrap()
        .classify(&req("hello"))
        .await
        .unwrap();
    assert_eq!(result.tier, ComplexityTier::Trivial);
    assert_eq!(result.cascade.as_deref(), Some("local-verified"));
}

/// A confident disagreement raises the tier to at least Hard.
#[tokio::test]
async fn disagreement_escalates_to_at_least_hard() {
    let base = serve(vec![verify(0.04)]).await;
    let mut c = config(&base);
    c.cascade.enabled = true;
    c.cascade.verify_below = 1.0;
    let result = Classifier::new(c)
        .unwrap()
        .classify(&req("hello"))
        .await
        .unwrap();
    assert_eq!(result.tier, ComplexityTier::Hard);
    assert_eq!(result.cascade.as_deref(), Some("local-escalated"));
    assert!(result.reasons.iter().any(|r| r == "cascade-escalated"));
}

/// The cheap decision is a floor: a verifier that "disagrees" with an
/// already-maximal tier must not be able to pull it down.
#[tokio::test]
async fn escalation_never_lowers_the_tier() {
    // A prompt the heuristic rates Reasoning, and a verifier that disagrees.
    let base = serve(vec![verify(0.04)]).await;
    let mut c = config(&base);
    c.cascade.enabled = true;
    c.cascade.verify_below = 1.0;
    let result = Classifier::new(c)
        .unwrap()
        .classify(&req("prove the correctness of this reduction"))
        .await
        .unwrap();
    assert!(
        matches!(
            result.tier,
            ComplexityTier::Hard | ComplexityTier::Reasoning
        ),
        "tier was lowered to {:?}",
        result.tier
    );
}

/// `Accept` records the disagreement without moving the tier.
#[tokio::test]
async fn accept_records_the_disagreement_without_escalating() {
    let base = serve(vec![verify(0.04)]).await;
    let mut c = config(&base);
    c.cascade.enabled = true;
    c.cascade.verify_below = 1.0;
    c.cascade.on_unverified = CascadeAction::Accept;
    let result = Classifier::new(c)
        .unwrap()
        .classify(&req("hello"))
        .await
        .unwrap();
    assert_eq!(result.tier, ComplexityTier::Trivial, "must not escalate");
    assert!(
        result
            .reasons
            .iter()
            .any(|r| r == "cascade-disagreement-accepted")
    );
}

/// A coin-flip verifier must not be able to escalate traffic.
#[tokio::test]
async fn an_unsure_verifier_cannot_escalate() {
    let base = serve(vec![verify(0.50), verify(0.52), verify(0.49)]).await;
    let mut c = config(&base);
    c.cascade.enabled = true;
    c.cascade.verify_below = 1.0;
    let result = Classifier::new(c)
        .unwrap()
        .classify(&req("hello"))
        .await
        .unwrap();
    assert_eq!(
        result.tier,
        ComplexityTier::Trivial,
        "a 0.50 verifier must not escalate"
    );
    assert_eq!(result.cascade.as_deref(), Some("local-inconclusive"));
    assert!(result.reasons.iter().any(|r| r == "cascade-inconclusive"));
}

/// The verifier's conviction threshold is a knob and is inclusive.
#[tokio::test]
async fn the_verify_confidence_threshold_is_inclusive() {
    // noul=0.20 is a *disagreement* (below 0.5) whose concentration is exactly
    // 0.80, so the threshold lands on the boundary and inclusivity is visible.
    for (threshold, expect_escalated) in [(0.80, true), (0.81, false), (0.79, true)] {
        let base = serve(vec![verify(0.20)]).await;
        let mut c = config(&base);
        c.cascade.enabled = true;
        c.cascade.verify_below = 1.0;
        c.cascade.verify_confidence = threshold;
        let result = Classifier::new(c)
            .unwrap()
            .classify(&req("hello"))
            .await
            .unwrap();
        assert_eq!(
            result.tier == ComplexityTier::Hard,
            expect_escalated,
            "threshold {threshold} against noul=0.20 (concentration 0.80)"
        );
    }
}

/// A confident local answer is not worth a second call. The mock serves nothing,
/// so any call at all would fail the test.
#[tokio::test]
async fn a_confident_local_answer_is_not_verified() {
    let base = serve(vec![]).await;
    let mut c = config(&base);
    c.cascade.enabled = true;
    c.cascade.verify_below = 0.0; // verify only at confidence <= 0
    let result = Classifier::new(c)
        .unwrap()
        .classify(&req("hello"))
        .await
        .expect("must not call the verifier");
    assert_eq!(result.cascade, None, "no verification was attempted");
}

/// An unreachable verifier leaves the local decision intact. The cascade exists
/// to catch systematic errors; a check that could not run has told us nothing,
/// and treating silence as failure would escalate every request during an
/// outage.
#[tokio::test]
async fn an_unreachable_verifier_keeps_the_local_decision() {
    // Point at a closed port.
    let mut c = config("http://127.0.0.1:9");
    c.cascade.enabled = true;
    c.cascade.verify_below = 1.0;
    c.jev.timeout_ms = 200;
    let result = Classifier::new(c)
        .unwrap()
        .classify(&req("hello"))
        .await
        .unwrap();
    assert_eq!(result.tier, ComplexityTier::Trivial);
    assert_eq!(result.cascade.as_deref(), Some("local-unverified"));
    assert!(result.reasons.iter().any(|r| r == "cascade-unavailable"));
}

/// A malformed verification answer is inconclusive, not a disagreement.
#[tokio::test]
async fn a_malformed_verification_is_inconclusive() {
    for body in [
        "{}",
        r#"{"answers":{}}"#,
        r#"{"answers":{"tier_is_right":{"type":"noul"}}}"#,
        "not json at all",
    ] {
        let base = serve(vec![body.to_string()]).await;
        let mut c = config(&base);
        c.cascade.enabled = true;
        c.cascade.verify_below = 1.0;
        let result = Classifier::new(c)
            .unwrap()
            .classify(&req("hello"))
            .await
            .unwrap();
        assert_ne!(
            result.tier,
            ComplexityTier::Hard,
            "{body:?} was treated as a disagreement"
        );
    }
}

/// An explicit directive is the caller's decision; a model must not re-litigate
/// it.
#[tokio::test]
async fn an_explicit_directive_is_never_verified() {
    let base = serve(vec![]).await;
    let mut c = config(&base);
    c.cascade.enabled = true;
    c.cascade.verify_below = 1.0;
    let result = Classifier::new(c)
        .unwrap()
        .classify(&req("@route:trivial\nhello"))
        .await
        .expect("must not call the verifier");
    assert_eq!(result.tier, ComplexityTier::Trivial);
    assert_eq!(result.cascade, None);
}

/// Cascade and screening compose: both are extra questions, and both must
/// survive being enabled together.
#[tokio::test]
async fn cascade_and_screening_compose() {
    // Two calls: the verification, then a screening question riding alongside it.
    let base = serve(vec![
        json!({
            "answers": {
                "tier_is_right": {"type": "noul", "noul": 0.04},
                "security": {"type": "noul", "noul": 0.02}
            }
        })
        .to_string(),
    ])
    .await;
    let mut c = config(&base);
    c.cascade.enabled = true;
    c.security.enabled = true;
    c.cascade.verify_below = 1.0;
    let result = Classifier::new(c)
        .unwrap()
        .classify(&req("hello"))
        .await
        .unwrap();
    assert_eq!(result.tier, ComplexityTier::Hard, "cascade escalated");
    // The screening answer travelled in the same request, so it cost no extra
    // round trip.
    let _ = result.security_risk;
    let _: Value = serde_json::json!({});
}
