use clap::Parser;
use miser_classifier::Classifier;
use miser_types::{ChatCompletionRequest, ClassifierConfig, ClassifierMode, ComplexityTier};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufRead, BufReader},
};

#[derive(Parser, Debug)]
struct Args {
    #[arg(long, default_value = "evals/cases.jsonl")]
    corpus: String,
    #[arg(long, default_value = "heuristic")]
    mode: String,
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
    let mode = match args.mode.as_str() {
        "heuristic" => ClassifierMode::Heuristic,
        "local_llm" => ClassifierMode::LocalLlm,
        "cloud_llm" => ClassifierMode::CloudLlm,
        _ => ClassifierMode::Hybrid,
    };
    let config = ClassifierConfig {
        mode,
        ..serde_json::from_str("{}").unwrap()
    };
    let classifier = Classifier::new(config)?;
    let reader = BufReader::new(File::open(args.corpus)?);
    let mut total = 0;
    let mut exact = 0;
    let mut adjacent = 0;
    let mut matrix: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let case: Case = serde_json::from_str(&line)?;
        let output = classifier.classify(&case.request).await?;
        total += 1;
        if output.tier == case.expected_tier {
            exact += 1;
        }
        if (output.tier as i32 - case.expected_tier as i32).abs() <= 1 {
            adjacent += 1;
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
    }
    println!(
        "exact_accuracy={:.4} adjacent_accuracy={:.4} cases={total}",
        exact as f64 / total as f64,
        adjacent as f64 / total as f64
    );
    println!("confusion={}", serde_json::to_string_pretty(&matrix)?);
    Ok(())
}
