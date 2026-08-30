//! Append-only per-request usage ledger and aggregation.
//!
//! Every chat completion (including cache hits) appends one
//! [`UsageRecord`] line to a JSONL file. Aggregation walks the file and
//! buckets by model, tier, key, client and day — the same shape as
//! OpenRouter's per-key activity view.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

/// One settled gateway request, attributed to its API key and client.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UsageRecord {
    /// Unix timestamp (seconds).
    pub ts: u64,
    pub key_id: String,
    /// Client/application label from the key ("-" when unset).
    pub client: String,
    pub model: String,
    pub tier: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost_usd: f64,
    pub latency_ms: u64,
    /// True when served from the exact-match response cache.
    pub cached: bool,
    pub status: u16,
    pub request_id: String,
}

/// Aggregate rollups over a window, serialised straight into the admin API.
#[derive(Clone, Debug, Default, Serialize)]
pub struct UsageSummary {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost_usd: f64,
    pub by_model: BTreeMap<String, ModelUsage>,
    pub by_tier: BTreeMap<String, u64>,
    pub by_key: BTreeMap<String, KeyUsage>,
    pub by_client: BTreeMap<String, ClientUsage>,
    /// Requests per UTC day (YYYY-MM-DD), oldest first.
    pub by_day: BTreeMap<String, DayUsage>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ModelUsage {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost_usd: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct KeyUsage {
    pub client: String,
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost_usd: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ClientUsage {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost_usd: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct DayUsage {
    pub requests: u64,
    pub cost_usd: f64,
}

pub struct UsageLedger {
    path: PathBuf,
    write_lock: Mutex<()>,
}

impl UsageLedger {
    pub fn new(path: PathBuf) -> Self {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        Self {
            path,
            write_lock: Mutex::new(()),
        }
    }

    /// Append one settled request. Never panics; failures are best-effort
    /// analytics (the response has already been produced).
    pub fn record(&self, record: &UsageRecord) {
        let Ok(line) = serde_json::to_string(record) else {
            return;
        };
        let _guard = self.write_lock.lock();
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(file, "{line}");
        }
    }

    /// Aggregate every recorded request in the window defined by the
    /// optional filters. `since_ts` filters by time; the other two filter
    /// by attribution.
    pub fn summarize(
        &self,
        since_ts: Option<u64>,
        key_id: Option<&str>,
        client: Option<&str>,
    ) -> UsageSummary {
        let mut summary = UsageSummary::default();
        let Ok(text) = fs::read_to_string(&self.path) else {
            return summary;
        };
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let Ok(record) = serde_json::from_str::<UsageRecord>(line) else {
                continue;
            };
            if let Some(since) = since_ts {
                if record.ts < since {
                    continue;
                }
            }
            if let Some(want) = key_id {
                if record.key_id != want {
                    continue;
                }
            }
            if let Some(want) = client {
                if record.client != want {
                    continue;
                }
            }
            summary.requests += 1;
            summary.prompt_tokens += record.prompt_tokens;
            summary.completion_tokens += record.completion_tokens;
            summary.cost_usd += record.cost_usd;

            let model = summary.by_model.entry(record.model.clone()).or_default();
            model.requests += 1;
            model.prompt_tokens += record.prompt_tokens;
            model.completion_tokens += record.completion_tokens;
            model.cost_usd += record.cost_usd;

            *summary.by_tier.entry(record.tier.clone()).or_default() += 1;

            let key = summary.by_key.entry(record.key_id.clone()).or_default();
            key.client = record.client.clone();
            key.requests += 1;
            key.prompt_tokens += record.prompt_tokens;
            key.completion_tokens += record.completion_tokens;
            key.cost_usd += record.cost_usd;

            let client_agg = summary.by_client.entry(record.client.clone()).or_default();
            client_agg.requests += 1;
            client_agg.prompt_tokens += record.prompt_tokens;
            client_agg.completion_tokens += record.completion_tokens;
            client_agg.cost_usd += record.cost_usd;

            let day = day_of(record.ts).unwrap_or_else(|| "unknown".into());
            let day_agg = summary.by_day.entry(day).or_default();
            day_agg.requests += 1;
            day_agg.cost_usd += record.cost_usd;
        }
        // Round costs so JSON output stays readable.
        summary.cost_usd = round2(summary.cost_usd);
        for v in summary.by_model.values_mut() {
            v.cost_usd = round2(v.cost_usd);
        }
        for v in summary.by_key.values_mut() {
            v.cost_usd = round2(v.cost_usd);
        }
        for v in summary.by_client.values_mut() {
            v.cost_usd = round2(v.cost_usd);
        }
        for v in summary.by_day.values_mut() {
            v.cost_usd = round2(v.cost_usd);
        }
        summary
    }
}

/// UTC calendar day (YYYY-MM-DD) for a unix timestamp.
fn day_of(ts: u64) -> Option<String> {
    let days = ts / 86_400;
    // Days since epoch to civil date (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}
