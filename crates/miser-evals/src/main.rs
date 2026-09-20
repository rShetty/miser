use clap::Parser;
use miser_classifier::Classifier;
use miser_types::{ChatCompletionRequest, ClassifierMode, ComplexityTier};
use serde::Deserialize;
mod quality;

use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufRead, BufReader},
};

#[derive(Parser, Debug)]
struct Args {
    #[arg(long, default_value = "evals/cases.jsonl")]
    corpus: String,
    #[arg(long)]
    mode: Option<String>,
    /// TOML gateway config supplying classifier endpoint settings
    /// (defaults to built-in empty config; heuristic-only runs need none).
    #[arg(long)]
    config: Option<String>,
    /// Parallel in-flight classifications. Results print in corpus order;
    /// latency percentiles under concurrency > 1 include queueing.
    #[arg(long, default_value_t = 1)]
    concurrency: usize,
    #[arg(long)]
    quality: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Case {
    id: String,
    expected_tier: ComplexityTier,
    request: ChatCompletionRequest,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if let Some(path) = args.quality.as_deref() {
        return quality::run(path);
    }
    let mode = args.mode.as_deref().map(|mode| match mode {
        "heuristic" => ClassifierMode::Heuristic,
        "local_llm" => ClassifierMode::LocalLlm,
        "cloud_llm" => ClassifierMode::CloudLlm,
        "jev" => ClassifierMode::Jev,
        _ => ClassifierMode::Hybrid,
    });
    let mut config = match args.config.as_deref() {
        Some(path) => {
            let raw = std::fs::read_to_string(path)?;
            let gateway: miser_types::GatewayConfig = toml::from_str(&raw)?;
            gateway.classifier
        }
        None => serde_json::from_str("{}").unwrap(),
    };
    if let Some(mode) = mode {
        config.mode = mode;
    }
    // Endpoint API keys come from the environment when not set in config.
    if config
        .jev
        .api_key
        .as_deref()
        .unwrap_or_default()
        .is_empty()
    {
        if let Ok(key) = std::env::var("JEV_API_KEY") {
            if !key.is_empty() {
                config.jev.api_key = Some(key);
            }
        }
    }
    if config
        .cloud_llm
        .api_key
        .as_deref()
        .unwrap_or_default()
        .is_empty()
    {
        if let Ok(key) = std::env::var("OPENROUTER_API_KEY") {
            if !key.is_empty() {
                config.cloud_llm.api_key = Some(key);
            }
        }
    }
    let classifier = Classifier::new(config)?;
    let classifier = std::sync::Arc::new(classifier);
    let mode_name = classifier.mode_name();
    let reader = BufReader::new(File::open(&args.corpus)?);
    let mut cases: Vec<Case> = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        cases.push(serde_json::from_str(&line)?);
    }
    let concurrency = args.concurrency.max(1);
    let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrency));
    let mut handles = Vec::with_capacity(cases.len());
    for case in cases {
        let classifier = classifier.clone();
        let permit = permits.clone().acquire_owned().await?;
        handles.push(tokio::spawn(async move {
            let output = classifier.classify(&case.request).await;
            drop(permit);
            output.map(|output| (case, output))
        }));
    }
    let mut indexed: Vec<Option<(Case, miser_types::ClassificationResult)>> =
        Vec::with_capacity(handles.len());
    for handle in handles {
        indexed.push(Some(handle.await??));
    }
    let mut outcomes: Vec<(ComplexityTier, ComplexityTier)> = Vec::with_capacity(indexed.len());
    let mut latency_ms: Vec<u64> = Vec::new();
    let mut fallbacks = 0usize;
    let mut input_tokens = 0u64;
    let mut output_tokens = 0u64;
    let mut matrix: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for slot in indexed.iter_mut() {
        let (case, output) = slot.take().expect("all results collected");
        latency_ms.push(output.latency_ms);
        if mode_name != "heuristic" && mode_name != "hybrid" && output.classifier == "heuristic" {
            fallbacks += 1;
        }
        if let Some(usage) = output.extra.get("usage") {
            input_tokens += usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            output_tokens += usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        }
        *matrix
            .entry(format!("{:?}", case.expected_tier))
            .or_default()
            .entry(format!("{:?}", output.tier))
            .or_default() += 1;
        println!(
            "{} expected={:?} predicted={:?} confidence={:.2} classifier={}",
            case.id, case.expected_tier, output.tier, output.confidence, output.classifier
        );
        outcomes.push((case.expected_tier, output.tier));
    }
    let metrics = Metrics::from_outcomes(&outcomes);
    println!(
        "exact_accuracy={:.4} adjacent_accuracy={:.4} under_routing={:.4} over_routing={:.4} mean_tier_distance={:.4} cases={total_cases} failures={fallbacks}",
        metrics.exact,
        metrics.adjacent,
        metrics.under_routing,
        metrics.over_routing,
        metrics.mean_distance,
        total_cases = metrics.total,
        fallbacks = fallbacks,
    );
    if !latency_ms.is_empty() {
        let (avg, p50, p95, p99) = latency_stats(&latency_ms);
        println!("latency_ms_avg={avg:.1} latency_ms_p50={p50} latency_ms_p95={p95} latency_ms_p99={p99}");
    }
    if input_tokens > 0 || output_tokens > 0 {
        println!("tokens_in={input_tokens} tokens_out={output_tokens}");
    }
    println!("confusion={}", serde_json::to_string_pretty(&matrix)?);
    Ok(())
}

fn latency_stats(latency_ms: &[u64]) -> (f64, u64, u64, u64) {
    let mut sorted = latency_ms.to_vec();
    sorted.sort_unstable();
    let pct = |rank: usize| sorted[rank.min(sorted.len() - 1)];
    let avg = sorted.iter().sum::<u64>() as f64 / sorted.len() as f64;
    (
        avg,
        pct(sorted.len() / 2),
        pct(sorted.len() * 95 / 100),
        pct(sorted.len() * 99 / 100),
    )
}

/// Ordinal tier metrics over (expected, predicted) pairs.
#[derive(Debug, Default)]
struct Metrics {
    total: usize,
    exact: f64,
    adjacent: f64,
    under_routing: f64,
    over_routing: f64,
    mean_distance: f64,
}

impl Metrics {
    fn from_outcomes(outcomes: &[(ComplexityTier, ComplexityTier)]) -> Self {
        if outcomes.is_empty() {
            return Self::default();
        }
        let mut exact = 0usize;
        let mut adjacent = 0usize;
        let mut under = 0usize;
        let mut over = 0usize;
        let mut distance = 0i64;
        for &(expected, predicted) in outcomes {
            let d = predicted as i32 - expected as i32;
            distance += (d as i64).abs();
            if d == 0 {
                exact += 1;
                adjacent += 1;
            } else {
                if d < 0 {
                    under += 1;
                } else {
                    over += 1;
                }
                if d.abs() <= 1 {
                    adjacent += 1;
                }
            }
        }
        let n = outcomes.len() as f64;
        Self {
            total: outcomes.len(),
            exact: exact as f64 / n,
            adjacent: adjacent as f64 / n,
            under_routing: under as f64 / n,
            over_routing: over as f64 / n,
            mean_distance: distance as f64 / n,
        }
    }
}

#[cfg(test)]
mod metrics_tests {
    use super::Metrics;
    use miser_types::ComplexityTier::*;

    #[test]
    fn perfect_classification() {
        let m = Metrics::from_outcomes(&[(Trivial, Trivial), (Hard, Hard)]);
        assert_eq!((m.exact, m.adjacent, m.under_routing, m.over_routing, m.mean_distance), (1.0, 1.0, 0.0, 0.0, 0.0));
    }

    #[test]
    fn under_and_over_routing_split() {
        let m = Metrics::from_outcomes(&[(Standard, Trivial), (Standard, Reasoning), (Simple, Simple)]);
        assert_eq!(m.exact, 1.0 / 3.0);
        assert_eq!(m.adjacent, 1.0 / 3.0);
        assert_eq!(m.under_routing, 1.0 / 3.0);
        assert_eq!(m.over_routing, 1.0 / 3.0);
        assert!((m.mean_distance - 4.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn empty_corpus_is_zeroed() {
        let m = Metrics::from_outcomes(&[]);
        assert_eq!(m.total, 0);
        assert_eq!(m.exact, 0.0);
    }

    #[test]
    fn latency_percentiles() {
        let stats = super::latency_stats(&[10, 20, 30, 40, 50, 60, 70, 80, 90, 100]);
        assert_eq!(stats.0, 55.0);
        assert_eq!(stats.1, 60);
        assert_eq!(stats.2, 100);
        assert_eq!(stats.3, 100);
    }
}
