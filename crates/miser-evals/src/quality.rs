use serde::Deserialize;
use serde_json::Value;
use std::{
    fs::File,
    io::{BufRead, BufReader},
};

#[derive(Debug, Deserialize)]
struct QualityCase {
    id: String,
    task: String,
    prompt: String,
    required: Vec<String>,
}

pub fn run(path: &str) -> anyhow::Result<()> {
    let reader = BufReader::new(File::open(path)?);
    let cases: Vec<QualityCase> = reader
        .lines()
        .map(|line| Ok(serde_json::from_str(&line?)?))
        .collect::<anyhow::Result<_>>()?;
    let mut total = 0.0;
    for case in &cases {
        let content = std::env::var(format!("MISER_QUALITY_OUTPUT_{}", case.id))
            .unwrap_or_else(|_| case.prompt.clone());
        let normalized = content.to_lowercase();
        let covered = case
            .required
            .iter()
            .filter(|term| normalized.contains(&term.to_lowercase()))
            .count();
        let mut score = covered as f32 / case.required.len().max(1) as f32;
        if case.task == "structured" && serde_json::from_str::<Value>(&content).is_err() {
            score *= 0.25;
        }
        total += score;
        println!(
            "{} score={:.3} coverage={}/{}",
            case.id,
            score,
            covered,
            case.required.len()
        );
    }
    println!(
        "quality_score={:.4} cases={}",
        total / cases.len().max(1) as f32,
        cases.len()
    );
    Ok(())
}
